use super::ctx::Ctx;
use super::fsio::write_atomic;
use super::validate::{assert_valid_sid, Refused};
use std::collections::BTreeSet;

pub async fn read_pins(ctx: &Ctx) -> std::io::Result<BTreeSet<String>> {
    match tokio::fs::read_to_string(ctx.claude_dir.join("cdash-pins.json")).await {
        Ok(json) => serde_json::from_str(&json).map_err(std::io::Error::other),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(e) => Err(e),
    }
}

pub async fn set_pin(ctx: &Ctx, sid: &str, pinned: bool) -> Result<(), Refused> {
    assert_valid_sid(sid)?;
    // Serialize read-modify-write so concurrent clients cannot lose another pin.
    let _guard = ctx.pin_lock.lock().await;
    let save = async {
        let mut pins = read_pins(ctx).await?;
        if pinned { pins.insert(sid.to_owned()); } else { pins.remove(sid); }
        let json = serde_json::to_string(&pins).map_err(std::io::Error::other)?;
        write_atomic(&ctx.claude_dir.join("cdash-pins.json"), &json, ".tmp").await
    };
    save.await.map_err(|e| Refused::Failed(format!("Could not save pinned conversations: {e}")))
}
