use std::{
    io::{self, Write},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};

const MAX_PROGRESS_UPDATES: usize = 10;
const MIN_OPERATIONS_FOR_PROGRESS: usize = 20;

enum ProgressOutput {
    Direct(Box<dyn Write + Send>),
    Asynchronous(mpsc::Sender<String>),
}

pub struct ProgressReporter {
    output: ProgressOutput,
    worker: Option<JoinHandle<()>>,
}

impl ProgressReporter {
    pub fn new<W: Write + Send + 'static>(writer: W) -> Self {
        Self {
            output: ProgressOutput::Direct(Box::new(writer)),
            worker: None,
        }
    }

    pub fn stderr() -> Self {
        Self::stderr_with(io::stderr())
    }

    pub fn stderr_with<W: Write + Send + 'static>(writer: W) -> Self {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut writer = writer;
            for line in receiver {
                let _ = writeln!(writer, "{line}");
            }
        });
        Self {
            output: ProgressOutput::Asynchronous(sender),
            worker: Some(worker),
        }
    }

    pub fn phase(
        &mut self,
        concurrency: usize,
        name: &'static str,
        expected: usize,
    ) -> ProgressPhase<'_> {
        self.write_line(format!(
            "benchmark: concurrency={concurrency} {name} started (0/{expected})"
        ));
        ProgressPhase {
            reporter: self,
            concurrency,
            name,
            expected,
            completed: 0,
        }
    }

    fn write_line(&mut self, line: String) {
        match &mut self.output {
            ProgressOutput::Direct(writer) => {
                let _ = writeln!(writer, "{line}");
            }
            ProgressOutput::Asynchronous(sender) => {
                let _ = sender.send(line);
            }
        }
    }
}

impl Drop for ProgressReporter {
    fn drop(&mut self) {
        let output = std::mem::replace(
            &mut self.output,
            ProgressOutput::Direct(Box::new(io::sink())),
        );
        drop(output);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub struct ProgressPhase<'a> {
    reporter: &'a mut ProgressReporter,
    concurrency: usize,
    name: &'static str,
    expected: usize,
    completed: usize,
}

impl ProgressPhase<'_> {
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
        && completed.is_multiple_of(expected.div_ceil(MAX_PROGRESS_UPDATES))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn reports_lifecycle_with_elapsed_times_and_bounded_progress() {
        let output = Arc::new(Mutex::new(Vec::new()));
        {
            let mut reporter = ProgressReporter::new(Capture(output.clone()));
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

        let output = String::from_utf8(output.lock().unwrap().clone()).unwrap();
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
    fn asynchronous_output_keeps_progress_off_the_runner_thread() {
        let output = Arc::new(Mutex::new(Vec::new()));
        {
            let mut reporter = ProgressReporter::stderr_with(Capture(output.clone()));
            let mut phase = reporter.phase(1, "measured", 20);
            for _ in 0..20 {
                phase.complete_operation();
            }
            phase.finish(Duration::ZERO);
        }
        let output = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(output.contains("measured started (0/20)"));
        assert!(output.contains("measured progress 2/20"));
        assert!(output.contains("measured completed (20/20)"));
    }

    #[test]
    fn does_not_report_each_operation_for_small_phases() {
        let output = Arc::new(Mutex::new(Vec::new()));
        {
            let mut reporter = ProgressReporter::new(Capture(output.clone()));
            let mut phase = reporter.phase(1, "warm-up", 4);
            for _ in 0..4 {
                phase.complete_operation();
            }
            phase.finish(Duration::ZERO);
        }
        let output = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(!output.contains(" progress "));
    }
}
