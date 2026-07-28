//! macOS 平台 gating。spec「Native macOS preview runtime」要求 macOS 15 以上，
//! 其他平台或較舊版本必須以非零狀態退出並指名最低支援平台。

use std::fmt;

/// 最低支援的 macOS major version。
pub const MIN_MACOS_MAJOR: u64 = 15;

#[derive(Debug, PartialEq, Eq)]
pub enum PlatformError {
    /// 非 macOS 作業系統；只在非 macOS target 建構，macOS build 視為 dead code。
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    UnsupportedOs,
    /// macOS 版本低於最低支援版本；附偵測到的版本字串。
    UnsupportedMacosVersion(String),
    /// 無法偵測 macOS 版本。
    VersionDetectionFailed(String),
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlatformError::UnsupportedOs => {
                write!(
                    f,
                    "collab requires macOS {MIN_MACOS_MAJOR} or later; this operating system is not supported"
                )
            }
            PlatformError::UnsupportedMacosVersion(version) => {
                write!(
                    f,
                    "collab requires macOS {MIN_MACOS_MAJOR} or later; detected macOS {version}"
                )
            }
            PlatformError::VersionDetectionFailed(reason) => {
                write!(
                    f,
                    "collab requires macOS {MIN_MACOS_MAJOR} or later; failed to detect macOS version: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for PlatformError {}

/// 從 `sw_vers -productVersion` 的輸出（如 "15.7.7"）取出 major version。
pub fn parse_macos_major_version(raw: &str) -> Option<u64> {
    raw.trim().split('.').next()?.parse().ok()
}

/// 純函式版本 gating，供單元測試使用。
pub fn is_supported_macos_major(major: u64) -> bool {
    major >= MIN_MACOS_MAJOR
}

/// 確認目前平台受支援：macOS 15+。
#[cfg(target_os = "macos")]
pub fn ensure_supported_platform() -> Result<(), PlatformError> {
    // SIMPLE: 以 sw_vers subprocess 偵測版本，stdlib 即可；若日後需要免
    // subprocess，可改用 NSProcessInfo operatingSystemVersion。
    let output = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .map_err(|e| PlatformError::VersionDetectionFailed(e.to_string()))?;
    if !output.status.success() {
        return Err(PlatformError::VersionDetectionFailed(format!(
            "sw_vers exited with {}",
            output.status
        )));
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let major = parse_macos_major_version(&raw).ok_or_else(|| {
        PlatformError::VersionDetectionFailed(format!("unparseable version {raw:?}"))
    })?;
    if is_supported_macos_major(major) {
        Ok(())
    } else {
        Err(PlatformError::UnsupportedMacosVersion(
            raw.trim().to_string(),
        ))
    }
}

#[cfg(not(target_os = "macos"))]
pub fn ensure_supported_platform() -> Result<(), PlatformError> {
    Err(PlatformError::UnsupportedOs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_product_version() {
        assert_eq!(parse_macos_major_version("15.7.7\n"), Some(15));
        assert_eq!(parse_macos_major_version("26.0"), Some(26));
        assert_eq!(parse_macos_major_version("14.6.1"), Some(14));
    }

    #[test]
    fn rejects_unparseable_version() {
        assert_eq!(parse_macos_major_version(""), None);
        assert_eq!(parse_macos_major_version("beta"), None);
    }

    #[test]
    fn gates_on_minimum_major_version() {
        assert!(!is_supported_macos_major(13));
        assert!(!is_supported_macos_major(14));
        assert!(is_supported_macos_major(15));
        assert!(is_supported_macos_major(26));
    }

    #[test]
    fn error_message_names_minimum_platform() {
        let message = PlatformError::UnsupportedMacosVersion("14.6.1".into()).to_string();
        assert!(message.contains("macOS 15"));
        assert!(message.contains("14.6.1"));

        let message = PlatformError::UnsupportedOs.to_string();
        assert!(message.contains("macOS 15"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn detects_current_platform_as_supported() {
        // 開發與 CI 環境依 spec 均為 macOS 15+，此測試同時驗證 sw_vers 偵測路徑。
        assert_eq!(ensure_supported_platform(), Ok(()));
    }
}
