use std::fmt;

/// Message source: either a server name or `nick[!user][@host]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub user: Option<String>,
    pub host: Option<String>,
}

impl Source {
    pub fn parse(s: &str) -> Source {
        let (rest, host) = match s.split_once('@') {
            Some((r, h)) => (r, Some(h.to_owned())),
            None => (s, None),
        };
        let (name, user) = match rest.split_once('!') {
            Some((n, u)) => (n, Some(u.to_owned())),
            None => (rest, None),
        };
        Source { name: name.to_owned(), user, host }
    }

    /// Heuristic: a source with no user/host and a dot is a server name.
    pub fn is_server(&self) -> bool {
        self.user.is_none() && self.host.is_none() && self.name.contains('.')
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        if let Some(u) = &self.user {
            write!(f, "!{u}")?;
        }
        if let Some(h) = &self.host {
            write!(f, "@{h}")?;
        }
        Ok(())
    }
}
