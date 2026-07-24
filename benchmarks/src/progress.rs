use std::{io::Write, time::Duration};

const MAX_PROGRESS_UPDATES: usize = 10;
const MIN_OPERATIONS_FOR_PROGRESS: usize = 20;

pub struct ProgressReporter<W> {
    writer: W,
}

impl<W: Write> ProgressReporter<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    pub fn phase(&mut self, concurrency: usize, name: &'static str, expected: usize) -> ProgressPhase<'_, W> {
        let _ = writeln!(
            self.writer,
            "benchmark: concurrency={concurrency} {name} started (0/{expected})"
        );
        ProgressPhase {
            reporter: self,
            concurrency,
            name,
            expected,
            completed: 0,
        }
    }

    fn write_line(&mut self, line: String) {
        let _ = writeln!(self.writer, "{line}");
    }
}

pub struct ProgressPhase<'a, W> {
    reporter: &'a mut ProgressReporter<W>,
    concurrency: usize,
    name: &'static str,
    expected: usize,
    completed: usize,
}

impl<W: Write> ProgressPhase<'_, W> {
    pub fn complete_operation(&mut self) {
        self.completed += 1;
        if should_report_progress(self.completed, self.expected) {
            self.reporter.write_line(format!(
                "benchmark: concurrency={} {} progress {}/{}",
                self.concurrency, self.name, self.completed, self.expected
            ));
        }
    }

    pub fn finish(self, elapsed: Duration) {
        self.reporter.write_line(format!(
            "benchmark: concurrency={} {} completed ({}/{}) elapsed={:.3}s",
            self.concurrency,
            self.name,
            self.completed,
            self.expected,
            elapsed.as_secs_f64()
        ));
    }
}

fn should_report_progress(completed: usize, expected: usize) -> bool {
    expected >= MIN_OPERATIONS_FOR_PROGRESS
        && completed < expected
        && completed % expected.div_ceil(MAX_PROGRESS_UPDATES) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_lifecycle_with_elapsed_times_and_bounded_progress() {
        let mut output = Vec::new();
        {
            let mut reporter = ProgressReporter::new(&mut output);
            let mut setup = reporter.phase(8, "setup", 100);
            for _ in 0..100 {
                setup.complete_operation();
            }
            setup.finish(Duration::from_millis(250));
            let mut warmup = reporter.phase(8, "warm-up", 4);
            for _ in 0..4 {
                warmup.complete_operation();
            }
            warmup.finish(Duration::from_millis(10));
            let mut measured = reporter.phase(8, "measured", 100);
            for _ in 0..100 {
                measured.complete_operation();
            }
            measured.finish(Duration::from_secs(1));
        }

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("concurrency=8 setup started (0/100)"));
        assert!(output.contains("setup completed (100/100) elapsed=0.250s"));
        assert!(output.contains("warm-up started (0/4)"));
        assert!(output.contains("warm-up completed (4/4) elapsed=0.010s"));
        assert!(output.contains("measured started (0/100)"));
        assert!(output.contains("measured completed (100/100) elapsed=1.000s"));
        assert_eq!(output.matches("measured progress").count(), 9);
        assert!(output.matches("progress").count() <= 18);
    }

    #[test]
    fn does_not_report_each_operation_for_small_phases() {
        let mut output = Vec::new();
        {
            let mut reporter = ProgressReporter::new(&mut output);
            let mut phase = reporter.phase(1, "warm-up", 4);
            for _ in 0..4 {
                phase.complete_operation();
            }
            phase.finish(Duration::ZERO);
        }
        assert!(!String::from_utf8(output).unwrap().contains(" progress "));
    }
}
