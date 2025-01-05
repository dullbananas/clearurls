use alloc::borrow::Cow;
use alloc::borrow::ToOwned;
use alloc::str::FromStr;
use alloc::string::String;
use alloc::vec::Vec;

use percent_encoding::percent_decode_str;
use regex::{Regex, RegexSet};
use serde::Deserialize;
use url::{form_urlencoded, Url};

use crate::deserialize_utils::{
    deserialize_map_as_vec, deserialize_regex, deserialize_regex_set, deserialize_regex_vec,
};
use crate::Error;

#[derive(Debug, Deserialize)]
pub(crate) struct Rules {
    #[serde(deserialize_with = "deserialize_map_as_vec")]
    pub(crate) providers: Vec<Provider>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Provider {
    #[serde(deserialize_with = "deserialize_regex")]
    pub(crate) url_pattern: Regex,
    #[serde(default, deserialize_with = "deserialize_regex_vec")]
    pub(crate) rules: Vec<Regex>,
    #[serde(default, deserialize_with = "deserialize_regex_vec")]
    pub(crate) raw_rules: Vec<Regex>,
    #[serde(default, deserialize_with = "deserialize_regex_vec")]
    pub(crate) referral_marketing: Vec<Regex>,
    #[serde(default, deserialize_with = "deserialize_regex_set")]
    pub(crate) exceptions: RegexSet,
    #[serde(default, deserialize_with = "deserialize_regex_vec")]
    pub(crate) redirections: Vec<Regex>,
}

impl Provider {
    pub(crate) fn remove_fields_from_url(
        &self,
        url: &Url,
        strip_referral_marketing: bool,
    ) -> Result<Url, Error> {
        if let Some(redirect) = self.get_redirection(url.as_str())? {
            let url = repeatedly_urldecode(redirect)?;
            return Ok(Url::from_str(&url)?);
        };
        let mut url = Cow::Borrowed(url.as_str());
        for r in &self.raw_rules {
            match r.replace_all(&url, "") {
                Cow::Borrowed(_) => {}
                Cow::Owned(new) => url = Cow::Owned(new),
            }
        }
        // clones the string
        let mut url = Url::from_str(&url)?;
        let mut fields: Vec<(Cow<'_, str>, Cow<'_, str>)> = url.query_pairs().collect();
        let fragments = url.fragment().unwrap_or("");
        let mut fragments: Vec<(Cow<'_, str>, Cow<'_, str>)> =
            form_urlencoded::parse(fragments.as_bytes()).collect();

        for r in self.get_rules(strip_referral_marketing) {
            fields.retain(|(k, _)| !is_full_match(r, k));
            fragments.retain(|(k, _)| !is_full_match(r, k));
        }
        let query = serialize_params(fields.iter());
        let fragment = serialize_params(fragments.iter());
        url.set_query(query.as_deref());
        url.set_fragment(fragment.as_deref());

        Ok(url)
    }

    pub(crate) fn match_url(&self, url: &str) -> bool {
        self.url_pattern.is_match(url) && !self.match_exception(url)
    }

    /// If this returns `Some(key)`, then `provider.match_url(url)` always returns false if `keys_from_url(url)` contains `key`. This is used to improve performance by eliminating most calls to `match_url`.
    pub(crate) fn get_key(&self) -> Option<String> {
        self
            .url_pattern
            .as_str()
            .replace(r"\/", "/")
            .replace(r"\-", "-")
            .strip_prefix(r"^https?://(?:[a-z0-9-]+\.)*?")
            .and_then(|s| key_iter(s, r"\.").next())
            .map(ToOwned::to_owned)
    }

    fn match_exception(&self, url: &str) -> bool {
        url == "javascript:void(0)" || self.exceptions.is_match(url)
    }

    fn get_redirection<'a>(&self, url: &'a str) -> Result<Option<&'a str>, Error> {
        for r in &self.redirections {
            if let Some(c) = r.captures(url) {
                let c = c
                    .get(1)
                    .ok_or_else(|| Error::RedirectionHasNoCapturingGroup(r.clone()))?;
                let s = c.as_str();
                return Ok(Some(s));
            }
        }
        Ok(None)
    }

    fn get_rules(&self, strip_referral_marketing: bool) -> impl Iterator<Item = &Regex> {
        if strip_referral_marketing {
            self.rules.iter().chain(self.referral_marketing.iter())
        } else {
            #[allow(clippy::iter_on_empty_collections)] // false positive, core::iter::empty() doesn't work because it has a different type
            self.rules.iter().chain([].iter())
        }
    }
}

/// See `Provider::key`
pub(crate) fn keys_from_url(url: &str) -> impl Iterator<Item = &str> {
    url
        .strip_prefix("http")
        .map(|s| s.strip_prefix('s').unwrap_or(s))
        .and_then(|s| s.strip_prefix("://"))
        .into_iter()
        .flat_map(|s| key_iter(s, "."))
}

fn key_iter<'a>(s: &'a str, delimiter: &'static str) -> impl Iterator<Item = &'a str> + 'a {
    s
        .split_inclusive(delimiter)
        .filter_map(move |s| s.strip_suffix(delimiter))
        .take_while(|&s| !s.is_empty() && s.chars().all(is_allowed_domain_char))
}

const fn is_allowed_domain_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
}

fn serialize_params<'a>(
    mut params: impl Iterator<Item = &'a (Cow<'a, str>, Cow<'a, str>)>,
) -> Option<String> {
    let first2: Vec<_> = params.by_ref().take(2).collect();
    let ret = match &first2[..] {
        [] => String::new(),
        [anchor] if anchor.1 == "" => anchor.0.clone().into_owned(),
        _ => form_urlencoded::Serializer::new(String::new())
            .extend_pairs(first2)
            .extend_pairs(params)
            .finish(),
    };
    Some(ret).filter(|r| !r.is_empty())
}

fn repeatedly_urldecode(s: &str) -> Result<Cow<'_, str>, Error> {
    let mut before = Cow::Borrowed(s);
    loop {
        let after = percent_decode_str(&before).decode_utf8()?;
        match after {
            Cow::Borrowed(_) => {
                // unchanged, so return now
                return if after.starts_with("http") {
                    Ok(before)
                } else {
                    Ok(Cow::Owned(["http://", &*after].join("")))
                };
            }
            Cow::Owned(after) => {
                before = Cow::Owned(after);
            }
        }
    }
}

fn is_full_match(regex: &Regex, haystack: &str) -> bool {
    regex
        .find(haystack)
        .is_some_and(|m| m.len() == haystack.len())
}
