//! Retain a bounded, self-contained vector scene for replacement outer attachments.

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use vivid_protocol::{
    messages,
    vector::{self, AssetRelease, Brush, Command, Frame, ImageAsset},
};

#[derive(Default)]
pub(super) struct VectorRetention {
    assets: BTreeMap<u64, Arc<[u8]>>,
    scene_assets: BTreeMap<u64, Arc<[u8]>>,
    frame: Option<Arc<[u8]>>,
}

impl VectorRetention {
    pub fn frame(&self) -> Option<Arc<[u8]>> {
        self.frame.clone()
    }

    pub fn bytes(&self) -> u64 {
        self.assets
            .values()
            .map(|body| body.len() as u64)
            .sum::<u64>()
            + self
                .scene_assets
                .iter()
                .filter(|(id, _)| !self.assets.contains_key(id))
                .map(|(_, body)| body.len() as u64)
                .sum::<u64>()
            + self.frame.as_ref().map_or(0, |body| body.len() as u64)
    }

    pub fn accept(&mut self, kind: u16, body: &[u8]) -> io::Result<()> {
        let invalid = |e: vector::InvalidScene| io::Error::new(io::ErrorKind::InvalidData, e.0);
        match kind {
            messages::VECTOR_ASSET => {
                let asset = ImageAsset::decode(body).map_err(invalid)?;
                let bytes: usize = self.assets.values().map(|body| body.len()).sum();
                if self.assets.len() >= vector::MAX_RETAINED_ASSETS
                    || body.len() > vector::MAX_RETAINED_ASSET_BYTES.saturating_sub(bytes)
                {
                    return Err(io::Error::other("vector retained asset budget exceeded"));
                }
                self.assets.insert(asset.id, Arc::from(body));
            }
            messages::VECTOR_ASSET_RELEASE => {
                let release = AssetRelease::decode(body).map_err(invalid)?;
                self.assets.remove(&release.id);
            }
            messages::VECTOR_FRAME => {
                let frame = Frame::decode(body).map_err(invalid)?;
                let mut assets = BTreeMap::new();
                for command in frame.canvas.commands() {
                    let asset = match command {
                        Command::Image { asset, .. }
                        | Command::Fill(_, Brush::Image { asset, .. })
                        | Command::Stroke(_, Brush::Image { asset, .. }, _)
                        | Command::StyledStroke(_, Brush::Image { asset, .. }, _) => Some(asset),
                        _ => None,
                    };
                    if let Some(id) = asset {
                        let body = self.assets.get(id).ok_or_else(|| {
                            io::Error::other("vector scene references absent asset")
                        })?;
                        assets.insert(*id, body.clone());
                    }
                }
                self.scene_assets = assets;
                self.frame = Some(Arc::from(body));
            }
            _ => return Err(io::Error::other("unexpected vector record")),
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Vec<(u16, Arc<[u8]>)> {
        let mut assets = self.scene_assets.clone();
        assets.extend(self.assets.iter().map(|(id, body)| (*id, body.clone())));
        let mut records = assets
            .into_values()
            .map(|body| (messages::VECTOR_ASSET, body))
            .collect::<Vec<_>>();
        if let Some(frame) = &self.frame {
            records.push((messages::VECTOR_FRAME, frame.clone()));
        }
        for id in self
            .scene_assets
            .keys()
            .filter(|id| !self.assets.contains_key(id))
        {
            records.push((messages::VECTOR_ASSET_RELEASE, Arc::from(id.to_be_bytes())));
        }
        records
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vivid_protocol::vector::{Canvas, Rect};

    #[test]
    fn released_displayed_assets_rehydrate_before_the_frame_then_release() {
        let mut retained = VectorRetention::default();
        let asset = ImageAsset {
            id: 1,
            width: 1,
            height: 1,
            rgba: vec![255, 0, 0, 255],
        }
        .encode()
        .unwrap();
        retained.accept(messages::VECTOR_ASSET, &asset).unwrap();
        let mut canvas = Canvas::new();
        canvas
            .push(Command::Image {
                asset: 1,
                rect: Rect::new(0., 0., 1., 1.).unwrap(),
                opacity: u16::MAX,
            })
            .unwrap();
        let frame = Frame {
            epoch: 1,
            revision: 1,
            canvas,
        }
        .encode()
        .unwrap();
        retained.accept(messages::VECTOR_FRAME, &frame).unwrap();
        retained
            .accept(
                messages::VECTOR_ASSET_RELEASE,
                &AssetRelease { id: 1 }.encode().unwrap(),
            )
            .unwrap();
        let records = retained.snapshot();
        assert_eq!(
            records.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(),
            [
                messages::VECTOR_ASSET,
                messages::VECTOR_FRAME,
                messages::VECTOR_ASSET_RELEASE
            ]
        );
        let mut restored = VectorRetention::default();
        for (kind, body) in records {
            restored.accept(kind, &body).unwrap();
        }
        assert_eq!(restored.frame(), retained.frame());
        assert!(
            restored.accept(messages::VECTOR_FRAME, &frame).is_err(),
            "new frames cannot reuse a released asset"
        );
        let empty = Frame {
            epoch: 1,
            revision: 2,
            canvas: Canvas::new(),
        }
        .encode()
        .unwrap();
        retained.accept(messages::VECTOR_FRAME, &empty).unwrap();
        assert_eq!(retained.snapshot().len(), 1);
        assert_eq!(retained.bytes(), empty.len() as u64);
    }
}
