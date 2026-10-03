//! Opt-in timing log for performance work.
//!
//! Set `ZK_PROFILE=<file>` to append one line per event to that file.

use std::{
    cell::RefCell,
    fs::File,
    io::Write as _,
    rc::Rc,
    sync::OnceLock,
    time::{Duration, Instant},
};

thread_local! {
    static LOG: RefCell<Option<File>> = const { RefCell::new(None) };
}

static ENABLED: OnceLock<bool> = OnceLock::new();

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        let Some(path) = std::env::var_os("ZK_PROFILE") else {
            return false;
        };
        match File::options().create(true).append(true).open(path) {
            Ok(file) => {
                LOG.with(|log| *log.borrow_mut() = Some(file));
                true
            }
            Err(_) => false,
        }
    })
}

pub fn log(line: impl AsRef<str>) {
    if enabled() {
        LOG.with(|log| {
            if let Some(file) = log.borrow_mut().as_mut() {
                let _ = writeln!(file, "{}", line.as_ref());
            }
        });
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.
}

/// Measures one frame: element building, then the time until painting.
#[derive(Clone)]
pub struct Frame {
    start: Instant,
    built: Rc<RefCell<Option<Duration>>>,
}

impl Frame {
    pub fn start() -> Option<Self> {
        enabled().then(|| Self {
            start: Instant::now(),
            built: Rc::new(RefCell::new(None)),
        })
    }

    pub fn built(&self) {
        *self.built.borrow_mut() = Some(self.start.elapsed());
    }

    pub fn painted(&self) {
        let built = self.built.borrow().unwrap_or_default();
        log(format!("frame build={:.2} paint={:.2}", ms(built), ms(self.start.elapsed())));
    }
}

pub fn time<R>(label: &str, f: impl FnOnce() -> R) -> R {
    if !enabled() {
        return f();
    }
    let start = Instant::now();
    let result = f();
    log(format!("{label}={:.2}", ms(start.elapsed())));
    result
}
