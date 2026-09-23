//! Docs sync from origin/<branch>.

use crate::Rag;
use anyhow::{bail, Result};

pub fn run(_rag: &mut Rag, _app: Option<&str>, _full: bool) -> Result<()> {
    bail!("nebula rag sync is not implemented yet")
}
