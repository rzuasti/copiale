//! The new chat picker: contacts filtered by a fuzzy query.

use std::ops::Range;

use copiale_core::{Contact, PhoneNumber};

use crate::fuzzy::fuzzy_match;

/// Most contacts the picker lists.
pub const MAX_CANDIDATES: usize = 8;

/// A contact as the picker lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate<'a> {
    pub contact: &'a Contact,
    /// Name, else number.
    pub label: String,
    /// Byte ranges of `label` the query matched.
    pub highlights: Vec<Range<usize>>,
    /// The number, when the label is the name.
    pub detail: Option<&'a str>,
    /// A chat with them exists.
    pub existing: bool,
}

/// Contacts matching `query`, best first; ties keep the contacts' order.
pub(super) fn candidates<'a>(
    contacts: &'a [Contact],
    query: &str,
    existing: impl Fn(&Contact) -> bool,
) -> Vec<Candidate<'a>> {
    let mut matched: Vec<_> = contacts
        .iter()
        .filter_map(|contact| {
            let phone = contact.phone.as_ref().map(PhoneNumber::as_str);
            let label = contact.name.as_deref().or(phone)?;
            let (score, highlights) = match fuzzy_match(query, label) {
                Some(m) => (m.score, m.ranges),
                // Numbers match too, unhighlighted when the name shows.
                None => (fuzzy_match(query, phone?)?.score, Vec::new()),
            };
            let candidate = Candidate {
                contact,
                label: label.to_owned(),
                highlights,
                detail: contact.name.as_ref().and(phone),
                existing: existing(contact),
            };
            Some((score, candidate))
        })
        .collect();
    matched.sort_by_key(|(score, _)| -score);
    matched
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|(_, candidate)| candidate)
        .collect()
}
