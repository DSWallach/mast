# Mast metrics

The tool reads a supplied offline copy. It does not create the copy, discover a
profile, or open Mast's live stores. A snapshot directory contains a SQLite
backup named `docs.sqlite3` and, optionally, a copied `journals` directory.
Keep snapshots and reports outside the repository; they can contain private
transcripts even though reports contain aggregates only.

For a local profile, prepare a new private directory with a SQLite backup and
copy journals before running the tool (do not copy SQLite files directly):

```sh
mkdir -p "$HOME/mast-metrics/snapshots/DATE" "$HOME/mast-metrics/reports"
sqlite3 -readonly "$HOME/.zeron/profiles/local/docs.sqlite3" ".backup '$HOME/mast-metrics/snapshots/DATE/docs.sqlite3'"
cp -R "$HOME/.zeron/profiles/local/journals" "$HOME/mast-metrics/snapshots/DATE/journals"
```

Replace `DATE` with a fresh directory name. Run from the Mast checkout:

```sh
cargo run -p mast-metrics -- --from "$HOME/mast-metrics/snapshots/DATE" --format json > "$HOME/mast-metrics/reports/DATE.json"
cargo run -p mast-metrics -- --from "$HOME/mast-metrics/snapshots/DATE" --format markdown > "$HOME/mast-metrics/reports/DATE.md"
cargo run -p mast-metrics -- --from "$HOME/mast-metrics/snapshots/DATE" --compare "$HOME/mast-metrics/reports/2026-11-05-baseline.json"
```

The default window ends at the current time and spans 14 days. For an older
snapshot, specify `--until` as the snapshot time; `--since` and `--until` take
UTC dates or RFC3339 timestamps, inclusive and exclusive respectively. Compare
equivalent windows and review the report's coverage notes. Snapshot copying
does not atomically capture journals and SQLite together. Keep access to the
snapshot restricted and choose a retention period appropriate for the data.
