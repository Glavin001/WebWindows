//! The Direct3D 10/11 half of the core.

use d3dgpu_proto::Command;

use crate::Core;

impl Core {
    pub(crate) fn command11(&mut self, cmd: Command, _shared: &[u8]) {
        self.warn(format!("unsupported command {cmd:?}"));
    }
}
