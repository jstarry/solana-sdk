//! The `logger` module configures `env_logger`
use {
    lazy_static::lazy_static,
    std::{
        cell::RefCell,
        collections::HashMap,
        env,
        io::Write,
        sync::{Arc, RwLock},
        thread::JoinHandle,
    },
};

lazy_static! {
    static ref LOGGER: Arc<RwLock<env_logger::Logger>> =
        Arc::new(RwLock::new(env_logger::Logger::from_default_env()));

    // Validator registry
    static ref VALIDATOR_REGISTRY: RwLock<Vec<String>> = RwLock::new(Vec::new());

    // Thread name pattern registry
    static ref THREAD_NAME_REGISTRY: RwLock<HashMap<String, usize>> = RwLock::new(HashMap::new());
}

// Thread-local validator log prefix
thread_local! {
    static LOG_PREFIX: RefCell<Option<String>> = RefCell::new(None);
}

pub const DEFAULT_FILTER: &str = "solana=info,agave=info";

struct LoggerShim {}

impl log::Log for LoggerShim {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        LOGGER.read().unwrap().enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        LOGGER.read().unwrap().log(record);
    }

    fn flush(&self) {}
}

fn replace_logger(logger: env_logger::Logger) {
    log::set_max_level(logger.filter());
    *LOGGER.write().unwrap() = logger;
    let _ = log::set_boxed_logger(Box::new(LoggerShim {}));
}

pub struct ValidatorGuard;

/// Register a new validator
#[must_use]
pub fn register_validator(validator_name: &str) -> ValidatorGuard {
    let mut validators = VALIDATOR_REGISTRY.write().unwrap();
    let validator_id = validators.len();
    validators.push(validator_name.to_string());

    // Update the calling thread's identity
    LOG_PREFIX.with(|prefix| {
        *prefix.borrow_mut() = Some(create_log_prefix(validator_id, validator_name));
    });

    ValidatorGuard
}

impl Drop for ValidatorGuard {
    fn drop(&mut self) {
        LOG_PREFIX.with(|prefix| {
            *prefix.borrow_mut() = Some("None".to_string());
        });
    }
}

fn create_log_prefix(validator_id: usize, validator_name: &str) -> String {
    // Format the prefix with first 3 characters uppercase
    let short_name = validator_name
        .chars()
        .take(3)
        .collect::<String>()
        .to_uppercase();

    // Create and store the log prefix for this thread
    format!("v{short_name}({validator_id})")
}

// Get log prefix for the current thread, initializing if needed
fn get_log_prefix() -> String {
    // Fast path - already registered
    let result = LOG_PREFIX.with(|prefix| prefix.borrow().clone());
    if let Some(prefix) = result {
        return prefix; // Already registered, super fast return
    }

    // Slow path - need to register this thread
    let thread_name = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();

    // Determine which validator this thread belongs to
    let (validator_id, validator_name) = {
        let validators = VALIDATOR_REGISTRY.read().unwrap();
        if validators.is_empty() {
            return "".to_string();
        }

        let mut registry = THREAD_NAME_REGISTRY.write().unwrap();
        let validator_id = registry.get(thread_name.as_str()).cloned();
        let next_id = validator_id.map(|id| id + 1).unwrap_or(0);
        if next_id >= validators.len() {
            return "".to_string();
        }
        registry.insert(thread_name.clone(), next_id);
        (next_id, validators[next_id].clone())
    };

    // Create and store the log prefix for this thread
    let prefix = create_log_prefix(validator_id, &validator_name);

    // Cache it in thread-local storage
    LOG_PREFIX.with(|p| {
        *p.borrow_mut() = Some(prefix.clone());
    });

    prefix
}

// Custom formatter function with the specified format and aligned level
fn custom_formatter(
    buf: &mut env_logger::fmt::Formatter,
    record: &log::Record,
) -> std::io::Result<()> {
    // Get thread-local validator prefix
    let prefix = get_log_prefix();

    // Start the log entry with a bracket
    write!(buf, "[")?;

    // Format timestamp
    write!(buf, "{}", buf.timestamp_nanos())?;

    // Add level in uppercase with fixed width (5 characters)
    // This ensures alignment: ERROR, WARN, INFO, DEBUG, TRACE
    write!(buf, " {:5}", record.level().to_string().to_uppercase())?;

    // Add validator prefix
    write!(buf, " {}", prefix)?;

    // Add module path if available
    if let Some(module_path) = record.module_path() {
        write!(buf, " {}", module_path)?;
    }

    // Close the header bracket and add the message
    write!(buf, "] {}", record.args())?;

    // Add newline
    writeln!(buf)
}

// Configures logging with a specific filter overriding RUST_LOG.  _RUST_LOG is used instead
// so if set it takes precedence.
// May be called at any time to re-configure the log filter
pub fn setup_with(filter: &str) {
    let logger =
        env_logger::Builder::from_env(env_logger::Env::new().filter_or("_RUST_LOG", filter))
            .format_timestamp_nanos()
            .format(custom_formatter)
            .build();
    replace_logger(logger);
}

// Configures logging with a default filter if RUST_LOG is not set
pub fn setup_with_default(filter: &str) {
    let logger = env_logger::Builder::from_env(env_logger::Env::new().default_filter_or(filter))
        .format_timestamp_nanos()
        .format(custom_formatter)
        .build();
    replace_logger(logger);
}

// Configures logging with the `DEFAULT_FILTER` if RUST_LOG is not set
pub fn setup_with_default_filter() {
    setup_with_default(DEFAULT_FILTER);
}

// Configures logging with the default filter "error" if RUST_LOG is not set
pub fn setup() {
    setup_with_default("error");
}

// Configures file logging with a default filter if RUST_LOG is not set
pub fn setup_file_with_default(logfile: &str, filter: &str) {
    use std::fs::OpenOptions;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(logfile)
        .unwrap();
    let logger = env_logger::Builder::from_env(env_logger::Env::new().default_filter_or(filter))
        .format_timestamp_nanos()
        .format(custom_formatter)
        .target(env_logger::Target::Pipe(Box::new(file)))
        .build();
    replace_logger(logger);
}

#[cfg(all(unix, not(target_arch = "wasm32")))]
fn redirect_stderr(filename: &str) {
    use std::{fs::OpenOptions, os::unix::io::AsRawFd};
    match OpenOptions::new().create(true).append(true).open(filename) {
        Ok(file) => unsafe {
            libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO);
        },
        Err(err) => eprintln!("Unable to open {filename}: {err}"),
    }
}

// Redirect stderr to a file with support for logrotate by sending a SIGUSR1 to the process.
//
// Upon success, future `log` macros and `eprintln!()` can be found in the specified log file.
#[cfg(not(target_arch = "wasm32"))]
pub fn redirect_stderr_to_file(logfile: Option<String>) -> Option<JoinHandle<()>> {
    // Default to RUST_BACKTRACE=1 for more informative validator logs
    if env::var_os("RUST_BACKTRACE").is_none() {
        env::set_var("RUST_BACKTRACE", "1")
    }

    match logfile {
        None => {
            setup_with_default_filter();
            None
        }
        Some(logfile) => {
            #[cfg(unix)]
            {
                use log::info;
                let mut signals =
                    signal_hook::iterator::Signals::new([signal_hook::consts::SIGUSR1])
                        .unwrap_or_else(|err| {
                            eprintln!("Unable to register SIGUSR1 handler: {err:?}");
                            std::process::exit(1);
                        });

                setup_with_default_filter();
                redirect_stderr(&logfile);
                Some(
                    std::thread::Builder::new()
                        .name("solSigUsr1".into())
                        .spawn(move || {
                            for signal in signals.forever() {
                                info!(
                                    "received SIGUSR1 ({}), reopening log file: {:?}",
                                    signal, logfile
                                );
                                redirect_stderr(&logfile);
                            }
                        })
                        .unwrap(),
                )
            }
            #[cfg(not(unix))]
            {
                println!("logrotate is not supported on this platform");
                setup_file_with_default(&logfile, DEFAULT_FILTER);
                None
            }
        }
    }
}
