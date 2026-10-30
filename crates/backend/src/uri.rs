//! Parsing and editing MongoDB connection strings without network access.
//!
//! This only splits a connection string into its parts so the connection
//! form can edit them. The driver validates options when connecting.

use std::fmt;

use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};

/// Characters that must be percent-encoded in usernames, passwords and the
/// database name.
const COMPONENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b':')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b']');

/// Characters that must be percent-encoded in option values.
const OPTION_VALUE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'=');

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionUri {
    pub srv: bool,
    pub username: Option<String>,
    pub password: Option<String>,
    /// `host` or `host:port`, as written.
    pub hosts: Vec<String>,
    /// The default auth database from the path.
    pub database: Option<String>,
    /// Option keys and values, percent-encoded as written, in order.
    options: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UriError(String);

impl fmt::Display for UriError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UriError {}

impl ConnectionUri {
    pub fn parse(uri: &str) -> Result<Self, UriError> {
        let error = |message: &str| Err(UriError(message.to_owned()));

        let Some((scheme, rest)) = uri.trim().split_once("://") else {
            return error("The connection string must start with mongodb:// or mongodb+srv://.");
        };
        let srv = match scheme {
            "mongodb" => false,
            "mongodb+srv" => true,
            _ => return error("The scheme must be mongodb:// or mongodb+srv://."),
        };

        let (before_query, query) = match rest.split_once('?') {
            Some((before, query)) => (before, Some(query)),
            None => (rest, None),
        };
        let (authority, path) = before_query.split_once('/').unwrap_or((before_query, ""));
        let (userinfo, hosts) = match authority.rsplit_once('@') {
            Some((userinfo, hosts)) => (Some(userinfo), hosts),
            None => (None, authority),
        };

        let hosts: Vec<String> = hosts
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(str::to_owned)
            .collect();
        if hosts.is_empty() {
            return error("The connection string has no host.");
        }
        if srv && hosts.len() > 1 {
            return error("mongodb+srv:// takes exactly one host.");
        }
        if srv && hosts[0].contains(':') {
            return error("mongodb+srv:// hosts can't have a port.");
        }

        let (username, password) = match userinfo {
            Some(userinfo) => {
                let (user, pass) = match userinfo.split_once(':') {
                    Some((user, pass)) => (user, Some(pass)),
                    None => (userinfo, None),
                };
                (Some(decode(user)?), pass.map(decode).transpose()?)
            }
            None => (None, None),
        };
        let database = (!path.is_empty()).then(|| decode(path)).transpose()?;

        let mut options = Vec::new();
        for pair in query
            .unwrap_or("")
            .split('&')
            .filter(|pair| !pair.is_empty())
        {
            let Some((key, value)) = pair.split_once('=') else {
                return Err(UriError(format!("Option \"{pair}\" has no value.")));
            };
            options.push((key.to_owned(), value.to_owned()));
        }

        Ok(Self {
            srv,
            username,
            password,
            hosts,
            database,
            options,
        })
    }

    /// Returns the decoded value of an option. Keys are case-insensitive.
    pub fn option(&self, key: &str) -> Option<String> {
        self.options
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .and_then(|(_, value)| decode(value).ok())
    }

    /// Sets an option, keeping its position if it exists. `None` removes it.
    pub fn set_option(&mut self, key: &str, value: Option<&str>) {
        let position = self
            .options
            .iter()
            .position(|(k, _)| k.eq_ignore_ascii_case(key));
        match (position, value) {
            (Some(index), Some(value)) => self.options[index].1 = encode_option(value),
            (None, Some(value)) => self.options.push((key.to_owned(), encode_option(value))),
            (Some(index), None) => {
                self.options.remove(index);
            }
            (None, None) => {}
        }
    }

    /// TLS is on by default for mongodb+srv:// and off for mongodb://.
    pub fn tls(&self) -> bool {
        self.option("tls")
            .or_else(|| self.option("ssl"))
            .map_or(self.srv, |value| value == "true")
    }

    pub fn set_tls(&mut self, enabled: bool) {
        self.set_option("ssl", None);
        let value = (enabled != self.srv).then_some(if enabled { "true" } else { "false" });
        self.set_option("tls", value);
    }

    pub fn without_password(&self) -> Self {
        Self {
            password: None,
            ..self.clone()
        }
    }
}

impl fmt::Display for ConnectionUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.srv {
            "mongodb+srv://"
        } else {
            "mongodb://"
        })?;
        if let Some(username) = &self.username {
            write!(f, "{}", utf8_percent_encode(username, COMPONENT))?;
            if let Some(password) = &self.password {
                write!(f, ":{}", utf8_percent_encode(password, COMPONENT))?;
            }
            f.write_str("@")?;
        }
        f.write_str(&self.hosts.join(","))?;
        if self.database.is_some() || !self.options.is_empty() {
            f.write_str("/")?;
        }
        if let Some(database) = &self.database {
            write!(f, "{}", utf8_percent_encode(database, COMPONENT))?;
        }
        for (index, (key, value)) in self.options.iter().enumerate() {
            let separator = if index == 0 { '?' } else { '&' };
            write!(f, "{separator}{key}={value}")?;
        }
        Ok(())
    }
}

fn decode(text: &str) -> Result<String, UriError> {
    percent_decode_str(text)
        .decode_utf8()
        .map(|decoded| decoded.into_owned())
        .map_err(|_| UriError(format!("\"{text}\" is not valid percent-encoded UTF-8.")))
}

fn encode_option(value: &str) -> String {
    utf8_percent_encode(value, OPTION_VALUE).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_parts() {
        let uri = ConnectionUri::parse(
            "mongodb://al%40ice:p%3Ass@db1:27017,db2:27018/admin?replicaSet=rs0&authSource=other",
        )
        .unwrap();

        assert!(!uri.srv);
        assert_eq!(uri.username.as_deref(), Some("al@ice"));
        assert_eq!(uri.password.as_deref(), Some("p:ss"));
        assert_eq!(uri.hosts, ["db1:27017", "db2:27018"]);
        assert_eq!(uri.database.as_deref(), Some("admin"));
        assert_eq!(uri.option("replicaset").as_deref(), Some("rs0"));
        assert_eq!(uri.option("authSource").as_deref(), Some("other"));
    }

    #[test]
    fn round_trips_unchanged() {
        for text in [
            "mongodb://localhost",
            "mongodb://localhost:27017/?directConnection=true",
            "mongodb+srv://user@cluster0.example.net/?appName=x",
            "mongodb://u%3A:p%40@h1,h2/db%2Fname?a=1&b=%26",
        ] {
            assert_eq!(ConnectionUri::parse(text).unwrap().to_string(), text);
        }
    }

    #[test]
    fn without_password_keeps_username() {
        let uri = ConnectionUri::parse("mongodb://user:secret@localhost/").unwrap();
        assert_eq!(
            uri.without_password().to_string(),
            "mongodb://user@localhost"
        );
    }

    #[test]
    fn rejects_invalid_strings() {
        for text in [
            "localhost:27017",
            "http://localhost",
            "mongodb://",
            "mongodb+srv://a.example.net,b.example.net",
            "mongodb+srv://a.example.net:27017",
            "mongodb://localhost/?novalue",
        ] {
            assert!(
                ConnectionUri::parse(text).is_err(),
                "{text} should be rejected"
            );
        }
    }

    #[test]
    fn tls_defaults_follow_scheme() {
        let mut plain = ConnectionUri::parse("mongodb://localhost").unwrap();
        let mut srv = ConnectionUri::parse("mongodb+srv://c.example.net").unwrap();
        assert!(!plain.tls());
        assert!(srv.tls());

        plain.set_tls(true);
        srv.set_tls(false);
        assert_eq!(plain.to_string(), "mongodb://localhost/?tls=true");
        assert_eq!(srv.to_string(), "mongodb+srv://c.example.net/?tls=false");

        plain.set_tls(false);
        assert_eq!(plain.to_string(), "mongodb://localhost");
    }

    #[test]
    fn set_option_encodes_and_keeps_position() {
        let mut uri = ConnectionUri::parse("mongodb://h/?a=1&b=2").unwrap();
        uri.set_option("A", Some("x&y"));
        uri.set_option("c", Some("3"));
        uri.set_option("b", None);
        assert_eq!(uri.to_string(), "mongodb://h/?a=x%26y&c=3");
        assert_eq!(uri.option("a").as_deref(), Some("x&y"));
    }
}
