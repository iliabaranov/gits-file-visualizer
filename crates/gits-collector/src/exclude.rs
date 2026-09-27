//! Exclude patterns: `~/x` and `/x` are anchored, `**/a/b` matches at any depth,
//! and `*` matches within one path component.

use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
struct Seg(Vec<String>);

impl Seg {
    fn new(s: &str) -> Self {
        Self(s.split('*').map(str::to_string).collect())
    }

    fn matches(&self, s: &str) -> bool {
        let parts = &self.0;
        if parts.len() == 1 {
            return parts[0] == s;
        }
        let (first, last) = (&parts[0], &parts[parts.len() - 1]);
        if !s.starts_with(first.as_str()) || !s.ends_with(last.as_str()) || s.len() < first.len() + last.len() {
            return false;
        }
        let mut rest = &s[first.len()..s.len() - last.len()];
        for mid in &parts[1..parts.len() - 1] {
            match rest.find(mid.as_str()) {
                Some(i) => rest = &rest[i + mid.len()..],
                None => return false,
            }
        }
        true
    }
}

#[derive(Clone, Debug)]
enum Rule {
    Anchored(Vec<Seg>),
    Anywhere(Vec<Seg>),
}

#[derive(Clone, Debug)]
pub struct Excludes {
    rules: Vec<Rule>,
}

fn components(p: &Path) -> Vec<String> {
    p.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

pub fn expand_home(s: &str, home: &Path) -> PathBuf {
    if s == "~" {
        home.to_path_buf()
    } else if let Some(rest) = s.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(s)
    }
}

impl Excludes {
    pub fn new(patterns: &[String], home: &Path) -> Self {
        let rules = patterns
            .iter()
            .filter(|p| !p.trim().is_empty())
            .map(|p| {
                let p = p.trim().trim_end_matches('/');
                if let Some(rest) = p.strip_prefix("**/") {
                    Rule::Anywhere(rest.split('/').map(Seg::new).collect())
                } else if p.starts_with('~') || p.starts_with('/') {
                    Rule::Anchored(components(&expand_home(p, home)).iter().map(|s| Seg::new(s)).collect())
                } else {
                    Rule::Anywhere(p.split('/').map(Seg::new).collect())
                }
            })
            .collect();
        Self { rules }
    }

    pub fn is_excluded(&self, path: &Path) -> bool {
        let comps = components(path);
        self.rules.iter().any(|r| match r {
            Rule::Anchored(segs) => segs.len() <= comps.len() && segs.iter().zip(&comps).all(|(s, c)| s.matches(c)),
            Rule::Anywhere(segs) => {
                segs.len() <= comps.len() && comps.windows(segs.len()).any(|w| segs.iter().zip(w).all(|(s, c)| s.matches(c)))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex() -> Excludes {
        let pats: Vec<String> =
            ["~/.cache", "**/node_modules", "**/.git/objects", "~/esp-idf*", "**/*.tmp"].iter().map(|s| s.to_string()).collect();
        Excludes::new(&pats, Path::new("/home/u"))
    }

    #[test]
    fn anchored_and_anywhere() {
        let e = ex();
        assert!(e.is_excluded(Path::new("/home/u/.cache")));
        assert!(e.is_excluded(Path::new("/home/u/.cache/mesa/x")));
        assert!(!e.is_excluded(Path::new("/home/u/src/.cache")));
        assert!(e.is_excluded(Path::new("/home/u/p/web/node_modules/react")));
        assert!(e.is_excluded(Path::new("/home/u/p/.git/objects/ab")));
        assert!(!e.is_excluded(Path::new("/home/u/p/.git/refs")));
        assert!(e.is_excluded(Path::new("/home/u/esp-idf-5.5/components")));
        assert!(!e.is_excluded(Path::new("/home/u/projects/esp-idf")));
        assert!(e.is_excluded(Path::new("/home/u/a/b/file.tmp")));
    }

    #[test]
    fn glob_segments() {
        assert!(Seg::new("a*c").matches("abbbc"));
        assert!(Seg::new("*.rs").matches("main.rs"));
        assert!(!Seg::new("*.rs").matches("main.rsx"));
        assert!(Seg::new("x*y*z").matches("x1y2z"));
        assert!(!Seg::new("ab*ba").matches("aba"));
    }
}
