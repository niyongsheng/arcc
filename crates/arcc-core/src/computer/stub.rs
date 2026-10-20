//! Non-macOS stub. Desktop control is a macOS-only feature in v1.

use arcc_storage::config::loader::ComputerUseConfig;

use super::{ComputerArgs, ComputerError, ComputerOutput, ComputerSession};

pub async fn execute(
    _session: &mut ComputerSession,
    _args: &ComputerArgs,
    _cfg: &ComputerUseConfig,
) -> Result<ComputerOutput, ComputerError> {
    Err(ComputerError::Unsupported)
}
