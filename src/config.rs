use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(name = "motion", about = "Private media catalog and Demuxe server")]
pub struct Args {
    /// JSON configuration. Relative paths in it are relative to the file.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Validate and print effective configuration without starting or modifying data.
    #[arg(long)]
    pub check_config: bool,
    /// Open the server in the default browser after startup (macOS).
    #[arg(long)]
    pub open_browser: bool,
    #[arg(long)]
    pub listen: Option<SocketAddr>,
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    /// Existing native catalog admitted by the completed offline cutover.
    #[arg(long)]
    pub catalog_path: Option<PathBuf>,
    /// Offline native39→40 transition: prepare a durable exact plan, or execute/reconcile it.
    #[arg(long, value_parser=["prepare", "execute"], requires="confirm_native_upgrade_quiescence", conflicts_with="check_config")]
    pub native_schema_upgrade: Option<String>,
    /// Confirm all other native/legacy readers and workers are stopped and exclusive operational ownership is retained.
    #[arg(long, requires = "native_schema_upgrade")]
    pub confirm_native_upgrade_quiescence: bool,
    /// Native source profile, checked against the retained cutover record.
    #[arg(long)]
    pub catalog_profile: Option<String>,
    #[arg(long)]
    pub public_origin: Option<String>,
    /// Overrides configuration libraries when provided; registers/scans at startup.
    #[arg(long)]
    pub library: Vec<PathBuf>,
    #[arg(long)]
    pub demuxe_dir: Option<PathBuf>,
    #[arg(long)]
    pub ffprobe: Option<PathBuf>,
    #[arg(long)]
    pub ffmpeg: Option<PathBuf>,
    /// Inherited file descriptor carrying a one-use desktop bootstrap secret
    /// (read once at startup; never place the secret in argv).
    #[arg(long)]
    pub bootstrap_fd: Option<i32>,
    /// Private inherited output pipe for native-host readiness (one JSON line).
    #[arg(long)]
    pub ready_fd: Option<i32>,
    /// Compatibility flag: Topcoat is always the production presentation.
    #[arg(long)]
    pub topcoat: bool,
    /// restricted (default): every request needs a credential. trusted_household:
    /// requests without one browse, play and save progress as the household
    /// viewer, never as an administrator. Retired APIs stay unavailable in both.
    #[arg(long, value_parser = parse_access_mode)]
    pub access_mode: Option<playscale_core::access::AccessMode>,
}

fn parse_access_mode(value: &str) -> Result<playscale_core::access::AccessMode, String> {
    serde_json::from_value(serde_json::Value::String(value.into()))
        .map_err(|_| "expected trusted_household or restricted".into())
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub catalog_path: Option<PathBuf>,
    pub catalog_profile: String,
    pub public_origin: Option<String>,
    pub libraries: Vec<PathBuf>,
    pub demuxe_dir: PathBuf,
    pub ffprobe: PathBuf,
    pub processing: crate::processing::Settings,
    pub storage: crate::storage::Settings,
    pub access_mode: playscale_core::access::AccessMode,
    pub api: crate::v2::ApiSettings,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8787".parse().unwrap(),
            data_dir: "data".into(),
            catalog_path: None,
            catalog_profile: "default".into(),
            public_origin: None,
            libraries: vec![],
            demuxe_dir: "web/vendor/demuxe".into(),
            ffprobe: "ffprobe".into(),
            processing: Default::default(),
            storage: Default::default(),
            // Retained household configuration cannot reopen retired routes.
            access_mode: playscale_core::access::AccessMode::Restricted,
            api: Default::default(),
        }
    }
}

/// Package locations are filesystem adapter configuration, not catalog state.
/// A marker distinguishes a relocatable package from a normal Cargo build.
pub fn packaged_defaults(executable: &Path, home: Option<&Path>) -> anyhow::Result<Settings> {
    // macOS current_exe can retain the outer CLI convenience symlink. Assets
    // belong beside its target inside Motion.app, not beside that symlink.
    let resolved;
    let executable = if executable.is_symlink() {
        resolved = std::fs::canonicalize(executable)?;
        resolved.as_path()
    } else {
        executable
    };
    let root = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("executable has no parent"))?;
    let marker = root.join("motion-package.json");
    let mut defaults = Settings::default();
    if !marker.try_exists()? {
        return Ok(defaults);
    }
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(marker)?)?;
    anyhow::ensure!(manifest["schema"] == 1, "unsupported Motion package schema");
    let home =
        home.ok_or_else(|| anyhow::anyhow!("HOME is required for the packaged data directory"))?;
    anyhow::ensure!(home.is_absolute(), "HOME must be absolute");
    defaults.data_dir = home.join("Library/Application Support/Motion");
    defaults.demuxe_dir = root.join("assets/demuxe");
    defaults.ffprobe = root.join("tools/ffprobe");
    defaults.processing.ffmpeg = root.join("tools/ffmpeg");
    for path in [
        &defaults.ffprobe,
        &defaults.processing.ffmpeg,
        &defaults.demuxe_dir.join("web/generated/player/index.js"),
    ] {
        anyhow::ensure!(
            path.is_file(),
            "incomplete Motion package: {}",
            path.display()
        );
    }
    Ok(defaults)
}
impl Args {
    pub fn resolve(&self) -> anyhow::Result<Settings> {
        self.resolve_with_defaults(Settings::default())
    }

    pub fn resolve_with_defaults(&self, defaults: Settings) -> anyhow::Result<Settings> {
        let mut settings = if let Some(path) = &self.config {
            let path = std::fs::canonicalize(path)?;
            anyhow::ensure!(
                std::fs::metadata(&path)?.len() <= 65536,
                "configuration exceeds 64 KiB"
            );
            let mut values = serde_json::to_value(&defaults)?;
            let supplied: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            let supplied = supplied
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("configuration must be an object"))?;
            for (key, value) in supplied {
                if key == "processing"
                    && let Some(fields) = value.as_object()
                {
                    for (name, value) in fields {
                        values["processing"][name] = value.clone();
                    }
                    continue;
                }
                values[key] = value.clone();
            }
            let mut s: Settings = serde_json::from_value(values)?;
            let root = path.parent().unwrap();
            for p in [&mut s.data_dir, &mut s.demuxe_dir] {
                if p.is_relative() {
                    *p = root.join(&*p);
                }
            }
            if let Some(p) = &mut s.catalog_path
                && p.is_relative()
            {
                *p = root.join(&*p);
            }
            for p in &mut s.libraries {
                if p.is_relative() {
                    *p = root.join(&*p);
                }
            }
            if s.ffprobe.is_relative() && s.ffprobe.components().count() > 1 {
                s.ffprobe = root.join(&s.ffprobe);
            }
            if s.processing.ffmpeg.is_relative() && s.processing.ffmpeg.components().count() > 1 {
                s.processing.ffmpeg = root.join(&s.processing.ffmpeg);
            }
            s
        } else {
            defaults
        };
        if let Some(v) = self.listen {
            settings.listen = v;
        }
        if let Some(v) = &self.data_dir {
            settings.data_dir = v.clone();
        }
        if let Some(v) = &self.catalog_path {
            settings.catalog_path = Some(v.clone());
        }
        if let Some(v) = &self.catalog_profile {
            settings.catalog_profile = v.clone();
        }
        anyhow::ensure!(
            !settings.catalog_profile.is_empty()
                && settings.catalog_profile.len() <= 256
                && !settings.catalog_profile.chars().any(char::is_control),
            "invalid native catalog profile"
        );
        if let Some(v) = &self.demuxe_dir {
            settings.demuxe_dir = v.clone();
        }
        if let Some(v) = &self.ffprobe {
            settings.ffprobe = v.clone();
        }
        if let Some(v) = &self.ffmpeg {
            settings.processing.ffmpeg = v.clone();
        }
        if let Some(v) = self.access_mode {
            settings.access_mode = v;
        }
        if let Some(v) = &self.public_origin {
            settings.public_origin = Some(v.clone());
        }
        if !self.library.is_empty() {
            settings.libraries = self.library.clone();
        }
        anyhow::ensure!(
            settings.listen.ip().is_loopback(),
            "Playscale must bind to loopback; expose it through Tailscale Serve"
        );
        if let Some(origin) = &settings.public_origin {
            settings.public_origin = Some(canonical_origin(origin)?);
        }
        settings.api.validate()?;
        settings.processing.validate()?;
        settings.storage.validate()?;
        Ok(settings)
    }
}
pub fn canonical_origin(origin: &str) -> anyhow::Result<String> {
    // Reject URL-parser repairs (whitespace, backslashes) at this security boundary.
    anyhow::ensure!(
        !origin
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\'),
        "invalid origin characters"
    );
    let uri: axum::http::Uri = origin.parse()?;
    anyhow::ensure!(
        uri.authority().is_some_and(|a| !a.as_str().contains('@'))
            && uri.path() == "/"
            && uri.query().is_none(),
        "origin cannot contain credentials, a path, or a query"
    );
    let url = url::Url::parse(origin)?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "public origin must be an HTTP(S) origin without credentials, path, query, or fragment"
    );
    Ok(url.origin().ascii_serialization())
}
pub fn validate_origin(origin: &str) -> anyhow::Result<()> {
    canonical_origin(origin).map(|_| ())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_catalog_config_is_explicit_and_resolves_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("motion.json");
        std::fs::write(
            &config,
            r#"{"catalog_path":"catalog.sqlite3","catalog_profile":"movies"}"#,
        )
        .unwrap();
        let args = Args::try_parse_from(["motion", "--config", config.to_str().unwrap()]).unwrap();
        let settings = args.resolve().unwrap();
        assert_eq!(
            settings.catalog_path,
            Some(dir.path().join("catalog.sqlite3"))
        );
        assert_eq!(settings.catalog_profile, "movies");
        assert_eq!(
            Args::try_parse_from(["motion"])
                .unwrap()
                .resolve()
                .unwrap()
                .catalog_path,
            None
        );
        let override_path = dir.path().join("override.sqlite3");
        let args = Args::try_parse_from([
            "motion",
            "--config",
            config.to_str().unwrap(),
            "--catalog-path",
            override_path.to_str().unwrap(),
            "--catalog-profile",
            "series",
        ])
        .unwrap();
        let settings = args.resolve().unwrap();
        assert_eq!(settings.catalog_path, Some(override_path));
        assert_eq!(settings.catalog_profile, "series");
    }

    #[test]
    fn native_catalog_profile_rejects_empty_and_controls() {
        for profile in ["", "bad\nprofile"] {
            let args = Args::try_parse_from(["motion", "--catalog-profile", profile]).unwrap();
            assert!(args.resolve().is_err());
        }
    }

    #[test]
    fn package_moves_preserve_assets_and_config_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Moved Motion With Spaces");
        std::fs::create_dir_all(root.join("assets/demuxe/web/generated/player")).unwrap();
        std::fs::create_dir(root.join("tools")).unwrap();
        std::fs::write(root.join("motion-package.json"), r#"{"schema":1}"#).unwrap();
        for name in [
            "tools/ffmpeg",
            "tools/ffprobe",
            "assets/demuxe/web/generated/player/index.js",
        ] {
            std::fs::write(root.join(name), "fixture").unwrap();
        }
        let home = dir.path().join("home");
        let defaults = packaged_defaults(&root.join("motion"), Some(&home)).unwrap();
        assert_eq!(
            defaults.data_dir,
            home.join("Library/Application Support/Motion")
        );
        let config = dir.path().join("config.json");
        std::fs::write(
            &config,
            r#"{"processing":{"timeout_seconds":30},"ffprobe":"custom/probe"}"#,
        )
        .unwrap();
        let args = Args::try_parse_from([
            "motion",
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            "state",
        ])
        .unwrap();
        let settings = args.resolve_with_defaults(defaults).unwrap();
        assert_eq!(settings.processing.ffmpeg, root.join("tools/ffmpeg"));
        assert_eq!(settings.processing.timeout_seconds, 30);
        assert_eq!(
            settings.ffprobe,
            dir.path().canonicalize().unwrap().join("custom/probe")
        );
        assert_eq!(settings.data_dir, PathBuf::from("state"));
        #[cfg(unix)]
        {
            let cli = dir.path().join("CLI");
            std::fs::create_dir(&cli).unwrap();
            std::fs::write(cli.join("motion-package.json"), r#"{"schema":1}"#).unwrap();
            std::fs::write(root.join("motion"), b"fixture executable").unwrap();
            std::os::unix::fs::symlink(root.join("motion"), cli.join("motion-server")).unwrap();
            let linked = packaged_defaults(&cli.join("motion-server"), Some(&home)).unwrap();
            assert_eq!(
                linked.ffprobe,
                root.canonicalize().unwrap().join("tools/ffprobe")
            );
            assert_eq!(
                linked.demuxe_dir,
                root.canonicalize().unwrap().join("assets/demuxe")
            );
        }
        std::fs::remove_file(root.join("tools/ffprobe")).unwrap();
        assert!(packaged_defaults(&root.join("motion"), Some(&home)).is_err());
    }
    #[test]
    fn browser_origin_canonicalization() {
        for (input, expected) in [
            ("https://EXAMPLE.com:443/", "https://example.com"),
            ("http://EXAMPLE.com:80", "http://example.com"),
            ("https://EXAMPLE.com:8443", "https://example.com:8443"),
            ("http://[0:0:0:0:0:0:0:1]:80", "http://[::1]"),
        ] {
            assert_eq!(canonical_origin(input).unwrap(), expected);
            let settings = Args::try_parse_from(["playscale", "--public-origin", input])
                .unwrap()
                .resolve()
                .unwrap();
            assert_eq!(settings.public_origin.as_deref(), Some(expected));
        }
        for input in [
            "https://example.com/#fragment",
            "https://example.com?",
            "https://user:pass@example.com",
            "https://example.com/path",
            " https://example.com",
            "https://example.com/../",
            "https://example.com\\evil",
        ] {
            assert!(canonical_origin(input).is_err(), "{input}");
        }
    }
    #[test]
    fn file_paths_and_explicit_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path,r#"{"data_dir":"state","demuxe_dir":"player","libraries":["media"],"ffprobe":"tools/ffprobe","public_origin":"https://host.example:8443"}"#).unwrap();
        let args = Args::try_parse_from([
            "playscale",
            "--config",
            path.to_str().unwrap(),
            "--listen",
            "127.0.0.1:9000",
        ])
        .unwrap();
        let s = args.resolve().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(s.data_dir, root.join("state"));
        assert_eq!(s.libraries, vec![root.join("media")]);
        assert_eq!(s.ffprobe, root.join("tools/ffprobe"));
        assert_eq!(s.listen.port(), 9000);
        let args = Args::try_parse_from([
            "playscale",
            "--config",
            path.to_str().unwrap(),
            "--data-dir",
            "override",
            "--library",
            "replacement",
        ])
        .unwrap();
        assert_eq!(args.resolve().unwrap().data_dir, PathBuf::from("override"));
        assert_eq!(
            args.resolve().unwrap().libraries,
            vec![PathBuf::from("replacement")]
        );
        std::fs::write(&path, r#"{"typo":true}"#).unwrap();
        assert!(args.resolve().is_err());
        assert!(
            Args::try_parse_from(["playscale", "--listen", "0.0.0.0:8787"])
                .unwrap()
                .resolve()
                .is_err()
        );
        for origin in [
            "https://host/path",
            "https://user@host",
            "ftp://host",
            "https://host?q=x",
        ] {
            assert!(validate_origin(origin).is_err());
        }
    }
}

#[cfg(test)]
mod native_upgrade_cli_tests {
    use super::*;
    #[test]
    fn offline_upgrade_requires_explicit_operator_confirmation() {
        assert!(Args::try_parse_from(["motion", "--native-schema-upgrade", "prepare"]).is_err());
        assert!(Args::try_parse_from(["motion", "--confirm-native-upgrade-quiescence"]).is_err());
        let args = Args::try_parse_from([
            "motion",
            "--native-schema-upgrade",
            "execute",
            "--confirm-native-upgrade-quiescence",
        ])
        .unwrap();
        assert_eq!(args.native_schema_upgrade.as_deref(), Some("execute"));
        assert!(args.confirm_native_upgrade_quiescence);
        assert!(
            Args::try_parse_from([
                "motion",
                "--native-schema-upgrade",
                "execute",
                "--confirm-native-upgrade-quiescence",
                "--check-config"
            ])
            .is_err()
        );
    }
}
