use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

use crate::config::{Config, Paths};

const CARBON_TOML: &str = r##"# paddock-theme 1
name = "carbon"

[colors]
bg = "#0a0a0a"
fg = "#d2d2c8"
accent = "#d8c070"
dim = "#666660"
unread = "#f2f2ea"
border = "#222220"
select = "#282828"
"##;

const PHOSPHOR_TOML: &str = include_str!("../themes/phosphor.toml");

#[derive(Debug, Clone, Deserialize)]
struct ThemeFile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    colors: ColorSlots,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ColorSlots {
    bg: Option<String>,
    fg: Option<String>,
    accent: Option<String>,
    dim: Option<String>,
    unread: Option<String>,
    border: Option<String>,
    select: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub bg: String,
    pub fg: String,
    pub accent: String,
    pub dim: String,
    pub unread: String,
    pub border: String,
    pub select: String,
}

impl Theme {
    pub fn carbon() -> Self {
        Self {
            name: "carbon".into(),
            bg: "#0a0a0a".into(),
            fg: "#d2d2c8".into(),
            accent: "#d8c070".into(),
            dim: "#666660".into(),
            unread: "#f2f2ea".into(),
            border: "#222220".into(),
            select: "#282828".into(),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let f: ThemeFile = toml::from_str(text).context("parse theme")?;
        let d = Self::carbon();
        Ok(Self {
            name: f.name.unwrap_or(d.name),
            bg: f.colors.bg.unwrap_or(d.bg),
            fg: f.colors.fg.unwrap_or(d.fg),
            accent: f.colors.accent.unwrap_or(d.accent),
            dim: f.colors.dim.unwrap_or(d.dim),
            unread: f.colors.unread.unwrap_or(d.unread),
            border: f.colors.border.unwrap_or(d.border),
            select: f.colors.select.unwrap_or(d.select),
        })
    }

    pub fn c_bg(&self) -> (u8, u8, u8) {
        parse_hex(&self.bg).unwrap_or((10, 10, 10))
    }
    pub fn c_fg(&self) -> (u8, u8, u8) {
        parse_hex(&self.fg).unwrap_or((210, 210, 200))
    }
    pub fn c_accent(&self) -> (u8, u8, u8) {
        parse_hex(&self.accent).unwrap_or((216, 192, 112))
    }
    pub fn c_dim(&self) -> (u8, u8, u8) {
        parse_hex(&self.dim).unwrap_or((102, 102, 96))
    }
    pub fn c_unread(&self) -> (u8, u8, u8) {
        parse_hex(&self.unread).unwrap_or((242, 242, 234))
    }
    pub fn c_border(&self) -> (u8, u8, u8) {
        parse_hex(&self.border).unwrap_or((34, 34, 32))
    }
    pub fn c_select(&self) -> (u8, u8, u8) {
        parse_hex(&self.select).unwrap_or((40, 40, 40))
    }

    pub fn css_vars(&self) -> String {
        format!(
            "--bg: {bg}; --fg: {fg}; --accent: {accent}; --dim: {dim}; --unread: {unread}; --border: {border}; --select: {select};",
            bg = self.bg,
            fg = self.fg,
            accent = self.accent,
            dim = self.dim,
            unread = self.unread,
            border = self.border,
            select = self.select,
        )
    }
}

pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim().trim_start_matches('#');
    if s.len() == 6 {
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some((r, g, b))
    } else if s.len() == 3 {
        let r = u8::from_str_radix(&s[0..1].repeat(2), 16).ok()?;
        let g = u8::from_str_radix(&s[1..2].repeat(2), 16).ok()?;
        let b = u8::from_str_radix(&s[2..3].repeat(2), 16).ok()?;
        Some((r, g, b))
    } else {
        None
    }
}

pub fn install_bundled(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    for (name, body) in [("carbon.toml", CARBON_TOML), ("phosphor.toml", PHOSPHOR_TOML)] {
        let p = dir.join(name);
        if !p.exists() {
            fs::write(&p, body).with_context(|| format!("write {}", p.display()))?;
        }
    }
    Ok(())
}

/// Name: override file, then `theme` in config.toml, else carbon.
/// File: `$config_dir/themes/<name>.toml`, else built-in carbon (or phosphor).
pub fn load_theme(config: &Config, paths: &Paths) -> Theme {
    let override_name = fs::read_to_string(paths.config_dir.join("theme"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let name = override_name
        .or_else(|| config.theme.clone())
        .unwrap_or_else(|| "carbon".into());
    load_named(&name, paths)
}

pub fn load_named(name: &str, paths: &Paths) -> Theme {
    let file = paths.config_dir.join("themes").join(format!("{name}.toml"));
    if let Ok(text) = fs::read_to_string(&file) {
        if let Ok(t) = Theme::parse(&text) {
            return t;
        }
    }
    if name == "phosphor" {
        if let Ok(t) = Theme::parse(PHOSPHOR_TOML) {
            return t;
        }
    }
    Theme::carbon()
}

pub fn write_theme_override(paths: &Paths, name: &str) -> Result<()> {
    fs::create_dir_all(&paths.config_dir)?;
    fs::write(paths.config_dir.join("theme"), name)?;
    Ok(())
}

pub fn list_themes(paths: &Paths) -> Vec<String> {
    let dir = paths.config_dir.join("themes");
    let mut names = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("toml") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.sort();
    if names.is_empty() {
        names.push("carbon".into());
        names.push("phosphor".into());
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carbon_slots() {
        let t = Theme::carbon();
        assert_eq!(t.bg, "#0a0a0a");
        assert_eq!(t.c_select(), (0x28, 0x28, 0x28));
        assert!(t.css_vars().contains("--accent: #d8c070"));
    }

    #[test]
    fn missing_slot_uses_carbon() {
        let t = Theme::parse("name = \"x\"\n[colors]\nfg = \"#00ff00\"\n").unwrap();
        assert_eq!(t.fg, "#00ff00");
        assert_eq!(t.bg, "#0a0a0a");
        assert_eq!(t.name, "x");
    }

    #[test]
    fn phosphor_file_parses() {
        let t = Theme::parse(PHOSPHOR_TOML).unwrap();
        assert_eq!(t.name, "phosphor");
        assert!(parse_hex(&t.fg).is_some());
    }

    #[test]
    fn hex3() {
        assert_eq!(parse_hex("#abc"), Some((0xaa, 0xbb, 0xcc)));
    }
}
