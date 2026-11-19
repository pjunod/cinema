//! Disk conversion's tool capture boundary. Native F+ execution stays Linux-only.

#[cfg(target_os = "linux")]
pub(crate) use super::dv_runtime::ToolSet;

#[cfg(not(target_os = "linux"))]
pub(crate) struct ToolSet {
    pub(crate) scanner: std::path::PathBuf,
    // No unsupported platform can construct a tool capability or a scanner
    // witness. Common publication code can type-check without inventing one.
    unavailable: std::convert::Infallible,
}

#[cfg(not(target_os = "linux"))]
impl ToolSet {
    pub(crate) async fn capture() -> Result<Option<std::sync::Arc<Self>>, String> {
        Err("F+ disk refitting is supported only on Linux".into())
    }

    pub(crate) fn refit_helper_identity(&self) -> String {
        match self.unavailable {}
    }

    pub(crate) async fn is_current(&self) -> bool {
        match self.unavailable {}
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn unsupported_platform_cannot_capture_a_disk_refit_capability() {
        assert!(matches!(super::ToolSet::capture().await,
            Err(error) if error == "F+ disk refitting is supported only on Linux"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_disk_refit_tools_are_the_original_native_type() {
        fn into_native(
            value: std::sync::Arc<super::ToolSet>,
        ) -> std::sync::Arc<super::super::dv_runtime::ToolSet> {
            value
        }
        fn from_native(
            value: std::sync::Arc<super::super::dv_runtime::ToolSet>,
        ) -> std::sync::Arc<super::ToolSet> {
            value
        }
        // Function pointer coercion checks identity without capturing or
        // launching a real native helper in this compile-time control.
        let _: fn(std::sync::Arc<super::ToolSet>) -> std::sync::Arc<super::ToolSet> = into_native;
        let _: fn(std::sync::Arc<super::ToolSet>) -> std::sync::Arc<super::ToolSet> = from_native;
    }
}
