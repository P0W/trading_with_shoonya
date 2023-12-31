// logger.rs
use colored::*;
use flexi_logger::{DeferredNow, Duplicate, FileSpec, LogSpecification, Logger, Record};

pub fn console_formatter(
    w: &mut dyn std::io::Write,
    now: &mut DeferredNow,
    record: &Record,
) -> std::io::Result<()> {
    let level = record.level();
    let color = match level {
        log::Level::Error => "red",
        log::Level::Warn => "yellow",
        log::Level::Info => "green",
        log::Level::Debug => "blue",
        log::Level::Trace => "bright black",
    };

    write!(
        w,
        "{}",
        format!(
            "{} [{}] {}:{}:{}: {}",
            now.now().format("%Y-%m-%d %H:%M:%S"),
            record.level(),
            record.file().unwrap_or("<unknown>"),
            record.line().unwrap_or(0),
            record.module_path().unwrap_or("<unknown>"),
            &record.args()
        )
        .color(color)
    )
}

pub fn file_formatter(
    w: &mut dyn std::io::Write,
    now: &mut DeferredNow,
    record: &Record,
) -> std::io::Result<()> {
    write!(
        w,
        "{} [{}] {}:{}:{}: {}",
        now.now().format("%Y-%m-%d %H:%M:%S"),
        record.level(),
        record.file().unwrap_or("<unknown>"),
        record.line().unwrap_or(0),
        record.module_path().unwrap_or("<unknown>"),
        &record.args()
    )
}

pub fn init_logger(prefix: &str, level: log::LevelFilter) {
    let log_spec: LogSpecification = LogSpecification::builder().default(level).build();

    Logger::with(log_spec)
        .format_for_stdout(console_formatter)
        .format_for_files(file_formatter)
        .log_to_file(FileSpec::default().directory("logs").basename(prefix))
        .duplicate_to_stdout(Duplicate::All)
        .print_message()
        .start()
        .unwrap_or_else(|e| panic!("Logger initialization failed with {}", e));

    log::debug!("Logger initialized");
}
