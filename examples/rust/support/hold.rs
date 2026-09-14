use std::{io, time::Duration};

/// Parse the optional finite lifetime before opening a connection.
pub fn duration(mut args: impl Iterator<Item = String>) -> io::Result<Option<Duration>> {
    match args.next() {
        None => Ok(None),
        Some(flag) if flag == "--duration" => {
            let seconds: f64 = args
                .next()
                .ok_or_else(|| io::Error::other("missing duration"))?
                .parse()
                .map_err(|_| io::Error::other("duration must be a number"))?;
            if !seconds.is_finite() || !(0.0..=3600.0).contains(&seconds) || args.next().is_some() {
                return Err(io::Error::other(
                    "duration must be 0..=3600 seconds; unexpected arguments",
                ));
            }
            Ok(Some(Duration::from_secs_f64(seconds)))
        }
        _ => Err(io::Error::other("usage: [--duration SECONDS]")),
    }
}

pub fn wait(duration: Option<Duration>) -> io::Result<()> {
    if let Some(duration) = duration {
        std::thread::sleep(duration);
    } else {
        eprintln!("Press Enter to remove the presentation.");
        io::stdin().read_line(&mut String::new())?;
    }
    Ok(())
}
