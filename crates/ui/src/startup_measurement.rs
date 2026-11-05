//! Optional timing for startup benchmark render work.

pub(crate) struct MeasurementSpan {
    name: &'static str,
    started: Option<(std::time::Instant, u128)>,
}

impl MeasurementSpan {
    pub(crate) fn new(name: &'static str) -> Self {
        let started = std::env::var_os("MAST_BENCH_RENDER").map(|_| {
            (
                std::time::Instant::now(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_micros(),
            )
        });
        Self { name, started }
    }
}

impl Drop for MeasurementSpan {
    fn drop(&mut self) {
        if let Some((started, wall)) = self.started {
            eprintln!(
                "BENCH render | {} | start_us={} duration_us={}",
                self.name,
                wall,
                started.elapsed().as_micros()
            );
        }
    }
}
