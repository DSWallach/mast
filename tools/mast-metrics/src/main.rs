use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use clap::{Parser, ValueEnum};
use mast_metrics::{
    metrics::{Report, measure},
    report::{comparison, markdown},
    snapshot::read_snapshot,
};

#[derive(Parser)]
#[command(about = "Aggregate metrics from an offline Mast snapshot")]
struct Arguments {
    /// Directory containing a SQLite .backup copy named docs.sqlite3.
    #[arg(long)]
    from: PathBuf,
    /// Inclusive UTC date or RFC3339 timestamp; default 14 days before until.
    #[arg(long)]
    since: Option<String>,
    /// Exclusive UTC date or RFC3339 timestamp; default current time.
    #[arg(long)]
    until: Option<String>,
    #[arg(long, value_enum, default_value_t = Format::Markdown)]
    format: Format,
    /// Compare against a prior JSON report; prints aggregate deltas.
    #[arg(long)]
    compare: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Json,
    Markdown,
}

fn parse_time(s: &str) -> Result<DateTime<Utc>> {
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(date.and_hms_opt(0, 0, 0).unwrap().and_utc());
    }
    Ok(DateTime::parse_from_rfc3339(s)
        .context("expected YYYY-MM-DD or RFC3339 timestamp")?
        .with_timezone(&Utc))
}

fn main() -> Result<()> {
    let args = Arguments::parse();
    let until = args
        .until
        .as_deref()
        .map(parse_time)
        .transpose()?
        .unwrap_or_else(Utc::now);
    let since = args
        .since
        .as_deref()
        .map(parse_time)
        .transpose()?
        .unwrap_or(until - Duration::days(14));
    if since >= until {
        bail!("since must be before until");
    }
    let snapshot = read_snapshot(&args.from)?;
    let report = measure(&snapshot, since, until)?;
    if let Some(path) = args.compare {
        let previous: Report = serde_json::from_reader(std::fs::File::open(path)?)?;
        anyhow::ensure!(
            previous.schema_version == report.schema_version,
            "incompatible report schema"
        );
        print!("{}", comparison(&previous, &report));
        return Ok(());
    }
    match args.format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        Format::Markdown => print!("{}", markdown(&report)),
    }
    Ok(())
}
