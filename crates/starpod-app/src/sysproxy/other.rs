//! Platforms without a system proxy backend. Applying fails with a clear
//! message, so the app keeps running and says why the setting did not take.

use super::{Backend, Plan, SavedSetting, SysproxyError};

pub fn backend() -> Box<dyn Backend> {
    Box::new(Unsupported)
}

struct Unsupported;

impl Backend for Unsupported {
    fn name(&self) -> &'static str {
        "unsupported"
    }

    fn capture(&self) -> Result<Vec<SavedSetting>, SysproxyError> {
        Ok(Vec::new())
    }

    fn apply(&self, _plan: &Plan) -> Result<Vec<String>, SysproxyError> {
        Err(SysproxyError(
            "the system proxy is not supported on this platform".into(),
        ))
    }

    fn restore(&self, _saved: &[SavedSetting]) -> Result<Vec<String>, SysproxyError> {
        Ok(Vec::new())
    }
}
