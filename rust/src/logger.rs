// logger.rs
use chrono::Local;
use flexi_logger::{DeferredNow, Duplicate, FileSpec, LogSpecification, Logger, Record};

pub fn my_formatter(
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
    let today = Local::now().format("%Y-%m-%d").to_string();

    Logger::with(log_spec)
        .format(my_formatter)
        .log_to_file(
            FileSpec::default()
                .directory("logs")
                .basename(prefix)
                .suffix("log")
                .suppress_timestamp()
                .discriminant(today),
        )
        // .duplicate_to_stderr(Duplicate::All)
        .duplicate_to_stdout(Duplicate::All)
        .print_message()
        .start()
        .unwrap_or_else(|e| panic!("Logger initialization failed with {}", e));

    log::info!("Logger initialized");
}
