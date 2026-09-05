use std::collections::BTreeMap;

use dataplane_ts::{TimeSeriesStore, TsPoint, TsinkTimeSeriesStore};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("dp-ts-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = TsinkTimeSeriesStore::new(&dir)?;

    let mut tags = BTreeMap::new();
    tags.insert("host".to_string(), "h1".to_string());
    store
        .write(TsPoint {
            measurement: "cpu_usage".to_string(),
            tags,
            field_name: "value".to_string(),
            field_value: 0.42,
            timestamp: 1_700_000_000_000_000,
        })
        .await?;

    let r = store
        .query_instant("cpu_usage{host=\"h1\"}", Some(1_700_000_000_000_000))
        .await?;
    println!("instant: {r:?}");

    let r = store
        .query_range(
            "cpu_usage",
            1_699_999_000_000_000,
            1_700_001_000_000_000,
            60,
        )
        .await?;
    println!("range: {r:?}");

    let err = store
        .query_instant("nosuchfn()", Some(0))
        .await
        .unwrap_err();
    println!("unknown-fn code: {}", err.code.as_str());

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
