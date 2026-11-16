//! One checked render, immutable copy and asynchronous retirement fence.

// Rust guideline compliant 2026-10-07
use super::*;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

// Match the existing bounded offscreen completion deadline, without extending it
// per tile or replacing the host's shared device when one job expires.
pub(super) const TIMEOUT: Duration = Duration::from_secs(10);
type RenderResult = (
    Cache,
    vivid_vello_backend::Renderer,
    Result<(), super::super::Error>,
);
type RenderFuture = Pin<Box<dyn Future<Output = RenderResult>>>;

enum Stage {
    Rendering {
        future: RenderFuture,
        lease: Lease,
        cancelled: Arc<AtomicBool>,
    },
    Copying {
        texture: Texture,
        done: mpsc::Receiver<()>,
    },
    Ready(Texture),
}

pub(super) struct Work {
    job: JobId,
    redrawn: bool,
    pub(super) started: Instant,
    stage: Stage,
}
impl Work {
    pub(super) fn is_rendering(&self) -> bool {
        matches!(self.stage, Stage::Rendering { .. })
    }
}

impl TextureRenderer {
    pub(super) fn start_job(
        &mut self,
        drawings: &[Drawing],
        rasters: &[RasterDrawing],
        width: u32,
        height: u32,
        budget: Option<WorkBudget>,
    ) -> io::Result<JobId> {
        if self.is_failed() {
            return Err(unavailable());
        }
        if self.is_busy() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "native texture work is pending",
            ));
        }
        super::super::validate(
            drawings,
            width,
            height,
            self.shared.device.limits().max_texture_dimension_2d,
        )
        .map_err(io::Error::other)?;
        super::super::validate_rasters(rasters).map_err(io::Error::other)?;
        let lease = self.pool.reserve()?;
        if self.renderer.is_none() {
            self.renderer = Some(
                vivid_vello_backend::Renderer::new(
                    &self.shared.device,
                    vivid_vello_backend::RendererOptions::default(),
                )
                .map_err(io::Error::other)?,
            );
        }
        let plan = self
            .cache
            .prepare(&self.shared.device, drawings, rasters, width, height)
            .map_err(io::Error::other)?;
        let job = JobId(Arc::new(()));
        let redrawn = plan.is_some();
        let stage = if let Some(plan) = plan {
            self.metrics = self.cache.metrics();
            let mut cache = std::mem::take(&mut self.cache);
            let mut renderer = self.renderer.take().expect("prepared texture renderer");
            let device = self.shared.device.clone();
            let queue = self.shared.queue.clone();
            let cancelled = Arc::new(AtomicBool::new(false));
            let worker_cancelled = cancelled.clone();
            Stage::Rendering {
                lease,
                cancelled,
                future: Box::pin(async move {
                    let result = cache
                        .checked_with_budget(
                            &device,
                            &queue,
                            &mut renderer,
                            plan,
                            &worker_cancelled,
                            budget.as_ref(),
                        )
                        .await;
                    (cache, renderer, result)
                }),
            }
        } else if let Some(output) = self.last.upgrade() {
            drop(lease);
            Stage::Ready(Texture(output))
        } else {
            self.copy_output(lease)
        };
        self.pending = Some(Work {
            job: job.clone(),
            redrawn,
            started: Instant::now(),
            stage,
        });
        Ok(job)
    }

    pub(super) fn poll_job(&mut self) -> io::Result<Option<Completed>> {
        // Polling the device can service callbacks without polling the render
        // future. Observe host health before allowing another fine submission.
        if let Err(error) = self.shared.device.poll(wgpu::PollType::Poll) {
            self.failed = true;
            self.cancel_job();
            return Err(io::Error::other(error));
        }
        if self.is_failed() {
            self.cancel_job();
            return Err(unavailable());
        }
        if let Some(done) = &self.retiring {
            match done.try_recv() {
                Ok(()) => self.retiring = None,
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.failed = true;
                    return Err(io::Error::other(
                        "native texture retirement callback was lost",
                    ));
                }
            }
        }
        let Some(mut work) = self.pending.take() else {
            return Ok(None);
        };
        if work.started.elapsed() >= TIMEOUT {
            self.pending = Some(work);
            self.cancel_job();
            self.failed = true;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native texture job timed out",
            ));
        }
        if let Stage::Rendering {
            mut future,
            lease,
            cancelled,
        } = work.stage
        {
            match future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
            {
                Poll::Pending => {
                    work.stage = Stage::Rendering {
                        future,
                        lease,
                        cancelled,
                    };
                    self.pending = Some(work);
                    return Ok(None);
                }
                Poll::Ready((cache, renderer, result)) => {
                    self.cache = cache;
                    self.renderer = Some(renderer);
                    self.metrics = self.cache.metrics();
                    if let Err(error) = result {
                        self.retire_lease(lease);
                        return Err(io::Error::other(error));
                    }
                    if self.is_failed() {
                        self.retire_lease(lease);
                        self.clear_cache();
                        return Err(unavailable());
                    }
                    work.stage = self.copy_output(lease);
                }
            }
        }
        let texture = match work.stage {
            Stage::Ready(texture) => texture,
            Stage::Copying { texture, done } => match done.try_recv() {
                Ok(()) => texture,
                Err(mpsc::TryRecvError::Empty) => {
                    work.stage = Stage::Copying { texture, done };
                    self.pending = Some(work);
                    return Ok(None);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.failed = true;
                    self.clear_cache();
                    return Err(io::Error::other("native texture copy callback was lost"));
                }
            },
            Stage::Rendering { .. } => unreachable!("pending render returned above"),
        };
        if self.is_failed() {
            self.clear_cache();
            return Err(unavailable());
        }
        self.last = Arc::downgrade(&texture.0);
        Ok(Some(Completed {
            job: work.job,
            texture,
            redrawn: work.redrawn,
        }))
    }

    fn copy_output(&self, lease: Lease) -> Stage {
        let target = self.cache.target.as_ref().expect("checked texture target");
        let texture = self.shared.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vivid.vector.immutable"),
            size: wgpu::Extent3d {
                width: target.width,
                height: target.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let mut encoder =
            self.shared
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("vivid.vector.immutable.copy"),
                });
        encoder.copy_texture_to_texture(
            target.texture.as_image_copy(),
            texture.as_image_copy(),
            texture.size(),
        );
        self.shared.queue.submit([encoder.finish()]);
        let output = Texture(Arc::new(Allocation {
            texture,
            queue: self.shared.queue.clone(),
            _lease: lease,
        }));
        let keep = output.clone();
        let (tx, done) = mpsc::sync_channel(1);
        self.shared.queue.on_submitted_work_done(move || {
            // Cancellation cannot free this charge before the GPU stops writing.
            drop(keep);
            let _ = tx.try_send(());
        });
        Stage::Copying {
            texture: output,
            done,
        }
    }

    fn retire_lease(&mut self, lease: Lease) {
        let (tx, done) = mpsc::sync_channel(1);
        self.shared.queue.on_submitted_work_done(move || {
            drop(lease);
            let _ = tx.try_send(());
        });
        self.retiring = Some(done);
    }

    fn clear_cache(&mut self) {
        self.cache.clear();
        self.last = Weak::new();
    }

    pub(super) fn cancel_job(&mut self) {
        let active = self.pending.is_some() || self.cache.target.is_some();
        if let Some(work) = self.pending.take() {
            match work.stage {
                Stage::Rendering {
                    future,
                    lease,
                    cancelled,
                } => {
                    cancelled.store(true, Ordering::Release);
                    // A queue completion also follows mapped-buffer callbacks.
                    // Keep output credit until those cancelled resources retire.
                    self.retire_lease(lease);
                    drop(future);
                    self.cache.metrics = self.metrics;
                }
                Stage::Copying { .. } | Stage::Ready(_) => {}
            }
        }
        self.clear_cache();
        if active && self.retiring.is_none() {
            let (tx, done) = mpsc::sync_channel(1);
            self.shared.queue.on_submitted_work_done(move || {
                let _ = tx.try_send(());
            });
            self.retiring = Some(done);
        }
    }
}
