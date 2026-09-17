//! Reusable, evidence-backed media labels. No inference service or media decoding.
use crate::channel_subjects::{digest, Metadata, SubjectDecision, Verdict};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const VERSION: &str = "metadata-rules-v1";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Label {
    pub dimension: String,
    pub value: String,
    pub source: String,
    pub evidence: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Classification {
    pub version: String,
    pub source_digest: String,
    pub keywords: Vec<String>,
    pub labels: Vec<Label>,
    pub provider_checked_at: i64,
    pub provider_error: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Overrides {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}
impl Overrides {
    pub fn validate(&mut self) -> Result<(), String> {
        if self.include.len() + self.exclude.len() > 100 {
            return Err("At most 100 classification corrections are allowed.".into());
        }
        for values in [&mut self.include, &mut self.exclude] {
            for v in values.iter_mut() {
                *v = v.trim().to_lowercase();
                let Some((dimension, value)) = v.split_once(':') else {
                    return Err("Use format:name, topic:name or genre:name.".into());
                };
                if !["format", "topic", "genre"].contains(&dimension)
                    || value.is_empty()
                    || v.len() > 100
                    || value.chars().any(char::is_control)
                {
                    return Err("Invalid classification label.".into());
                }
            }
            values.sort();
            values.dedup();
        }
        if self.include.iter().any(|v| self.exclude.contains(v)) {
            return Err("A label cannot be both included and excluded.".into());
        }
        Ok(())
    }
}
impl Classification {
    pub fn terms(&self, edits: &Overrides) -> Vec<String> {
        let mut result = self
            .labels
            .iter()
            .map(|l| format!("{}:{}", l.dimension, l.value))
            .chain(edits.include.iter().cloned())
            .filter(|v| !edits.exclude.contains(v))
            .collect::<BTreeSet<_>>();
        result.extend(self.keywords.iter().cloned());
        result.into_iter().collect()
    }
}
pub fn normalized(s: &str) -> String {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
fn has(text: &str, phrase: &str) -> bool {
    format!(" {text} ").contains(&format!(" {} ", normalized(phrase)))
}
/// A finite taxonomy, backed by provider keywords or explicit description evidence.
/// Unknown formats stay unknown; a generic Comedy genre never means stand-up.
pub fn classify(metadata: &Metadata, keywords: Vec<String>) -> Classification {
    let mut result = Classification {
        version: VERSION.into(),
        source_digest: digest(&metadata.fields),
        keywords,
        ..Default::default()
    };
    let title = metadata.fields.get("title").cloned().unwrap_or_default();
    let overview = metadata.fields.get("overview").cloned().unwrap_or_default();
    let text = normalized(&format!("{title} {overview}"));
    let genres = metadata.fields.get("genres").cloned().unwrap_or_default();
    let keyword_evidence = |aliases: &[&str]| {
        result
            .keywords
            .iter()
            .find(|k| aliases.iter().any(|a| normalized(k) == normalized(a)))
            .cloned()
    };
    let mut labels = Vec::new();
    let mut add = |dimension: &str, value: &str, source: &str, evidence: String| {
        labels.push(Label {
            dimension: dimension.into(),
            value: value.into(),
            source: source.into(),
            evidence,
        })
    };
    for (format, aliases) in [
        (
            "stand-up",
            &["stand-up comedy", "standup comedy", "stand-up special"][..],
        ),
        ("documentary", &["documentary"][..]),
        ("sitcom", &["sitcom", "situation comedy"][..]),
        ("talk-show", &["talk show"][..]),
        ("concert", &["concert film", "live concert"][..]),
        ("animation", &["animation", "animated film"][..]),
    ] {
        if let Some(k) = keyword_evidence(aliases) {
            add("format", format, "tmdb-keyword", k);
            continue;
        }
        if format == "documentary" && has(&normalized(&genres), "documentary") {
            add("format", format, "genre", "Documentary".into());
            continue;
        }
        if format == "animation" && has(&normalized(&genres), "animation") {
            add("format", format, "genre", "Animation".into());
            continue;
        }
        // Avoid turning fiction about a comic, interviews or backstage documentaries into performances.
        let contrary = [
            "aspiring comedian",
            "aspiring comic",
            "documentary about",
            "sitcom",
            "interview",
            "talk show",
            "backstage",
        ]
        .iter()
        .any(|p| has(&text, p));
        let phrases = if format == "stand-up" {
            &[
                "stand-up special",
                "standup special",
                "stand-up performance",
                "stand-up comedy special",
                "performs stand-up",
                "stand-up routine",
                "stand-up set",
            ][..]
        } else {
            aliases
        };
        if !(format == "stand-up" && contrary) {
            if let Some(p) = phrases.iter().find(|p| has(&text, p)) {
                add("format", format, "description", p.to_string());
            }
        }
    }
    for (topic, aliases) in [
        (
            "space",
            &[
                "space exploration",
                "astronomy",
                "astronaut",
                "spaceflight",
                "outer space",
                "solar system",
            ][..],
        ),
        (
            "politics",
            &["politics", "political", "election", "government"][..],
        ),
        ("cooking", &["cooking", "chef", "culinary", "cuisine"][..]),
        (
            "nature",
            &["wildlife", "natural history", "ecosystem", "nature"][..],
        ),
        ("music", &["music", "musician", "concert"][..]),
        ("history", &["history", "historical", "ancient"][..]),
        (
            "science",
            &["science", "scientific", "physics", "biology"][..],
        ),
        ("travel", &["travel", "travelogue", "tourism"][..]),
        (
            "sports",
            &[
                "sports",
                "sport",
                "athlete",
                "football",
                "basketball",
                "baseball",
            ][..],
        ),
    ] {
        if let Some(k) = keyword_evidence(aliases) {
            add("topic", topic, "tmdb-keyword", k);
        } else if let Some(p) = aliases.iter().find(|p| has(&text, p)) {
            add("topic", topic, "description", p.to_string());
        }
    }
    for genre in genres.split(", ").filter(|v| !v.trim().is_empty()) {
        add(
            "genre",
            &genre.trim().to_lowercase(),
            "genre",
            genre.to_owned(),
        );
    }
    result.labels = labels;
    result
}
/// Local subject grammar: whitespace AND, comma-separated alternatives, minus
/// exclusions, quoted phrases and format/topic/genre selectors. Legacy shipped
/// preset descriptions map to their equivalent finite rules, not arbitrary NLP.
pub fn subject_query(subject: &str) -> String {
    match subject {
        "Stand-up comedy performances and specials. Exclude sitcoms, comedy movies, talk shows, and documentaries about comedians."=>"format:stand-up -format:sitcom -format:talk-show -format:documentary".into(),
        "Documentaries about space exploration, astronomy, and the universe."=>"format:documentary topic:space".into(),
        "Comedy films and television episodes released between 1990 and 1999."=>"genre:comedy year:1990-1999".into(),
        "Film noir: crime stories with morally ambiguous characters and a dark, fatalistic style."=>"noir".into(),
        _=>subject.to_owned(),
    }
}
pub fn query_parts(query: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut quoted = false;
    for c in query.chars().take(1000) {
        if c == '"' {
            quoted = !quoted;
        } else if c.is_whitespace() && !quoted {
            if !part.is_empty() {
                parts.push(std::mem::take(&mut part));
            }
        } else {
            part.push(c);
        }
    }
    if !part.is_empty() {
        parts.push(part);
    }
    parts
}
pub fn local_decision(subject: &str, metadata: &Metadata) -> SubjectDecision {
    let query = subject_query(subject).to_lowercase();
    let text = normalized(
        &metadata
            .fields
            .values()
            .cloned()
            .collect::<Vec<_>>()
            .join(" "),
    );
    let tags = metadata.fields.get("tags").cloned().unwrap_or_default();
    let genres = normalized(
        metadata
            .fields
            .get("genres")
            .map(String::as_str)
            .unwrap_or_default(),
    );
    let generated = classify(metadata, Vec::new());
    let automatic = if tags.split(", ").any(|s| s == "classification:ready") {
        Vec::new()
    } else {
        generated.terms(&Overrides::default())
    };
    let mut evidence = String::new();
    let matches = |part: &str| {
        if let Some(year) = part.strip_prefix("year:") {
            let (lo, hi) = year.split_once('-').unwrap_or((year, year));
            return metadata
                .fields
                .get("year")
                .and_then(|s| s.parse::<i32>().ok())
                .zip(lo.parse::<i32>().ok().zip(hi.parse::<i32>().ok()))
                .is_some_and(|(y, (lo, hi))| y >= lo && y <= hi);
        }
        if let Some(genre) = part.strip_prefix("genre:") {
            return if tags.split(", ").any(|s| s == "classification:ready") {
                tags.split(", ").any(|s| s == part)
            } else {
                has(&genres, genre)
            };
        }
        if part.starts_with("format:") || part.starts_with("topic:") {
            return tags.split(", ").any(|s| s == part) || automatic.iter().any(|s| s == part);
        }
        part.split(',').any(|term| {
            let term = match term {
                "standup" => "stand up",
                "scifi" => "science fiction",
                _ => term,
            };
            has(&text, term) || automatic.iter().any(|s| has(&normalized(s), term))
        })
    };
    let parts = query_parts(&query);
    let matched = !parts.is_empty()
        && parts.iter().all(|p| {
            if let Some(excluded) = p.strip_prefix('-') {
                !matches(excluded)
            } else {
                matches(p)
            }
        });
    if matched {
        evidence = parts
            .iter()
            .filter(|p| !p.starts_with('-'))
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
    }
    SubjectDecision {
        verdict: if matched {
            Verdict::Match
        } else {
            Verdict::NoMatch
        },
        reason: if matched {
            format!("Matches local rules: {evidence}")
        } else {
            "Does not match the local search rules.".into()
        },
        evidence_field: "local_rules".into(),
        evidence_quote: evidence,
    }
}

/// Safe FTS5 grammar shared by both stores. Quotes are phrases, commas are
/// alternatives within a term, and a leading minus excludes a term.
pub fn fts_query(input: &str) -> Option<String> {
    let mut positive = Vec::new();
    let mut negative = Vec::new();
    let parts = query_parts(input);
    for (index, part) in parts.iter().enumerate() {
        let excluded = part.starts_with('-');
        let part = part.strip_prefix('-').unwrap_or(part);
        let alternatives = part
            .split(',')
            .filter_map(|term| {
                let term = normalized(term);
                if term.is_empty() {
                    return None;
                }
                let alias = match term.as_str() {
                    "standup" => Some("stand up"),
                    "scifi" => Some("science fiction"),
                    _ => None,
                };
                let prefix = if !excluded
                    && index + 1 == parts.len()
                    && !term.contains(' ')
                    && !input.trim_end().ends_with('"')
                {
                    "*"
                } else {
                    ""
                };
                Some(if let Some(alias) = alias {
                    format!("(\"{term}\"{prefix} OR \"{alias}\")")
                } else {
                    format!("\"{term}\"{prefix}")
                })
            })
            .collect::<Vec<_>>();
        if alternatives.is_empty() {
            continue;
        }
        let expression = format!("({})", alternatives.join(" OR "));
        if excluded {
            negative.push(expression);
        } else {
            positive.push(expression);
        }
    }
    if positive.is_empty() {
        return None;
    }
    let mut result = positive.join(" AND ");
    for excluded in negative {
        result = format!("({result}) NOT {excluded}");
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn metadata(text: &str) -> Metadata {
        Metadata {
            fields: [
                ("title".into(), "Example".into()),
                ("overview".into(), text.into()),
                ("genres".into(), "Comedy".into()),
            ]
            .into(),
            missing_overview: false,
            truncated: false,
        }
    }
    #[test]
    fn corrected_labels_are_not_reintroduced_from_the_description() {
        let mut m = metadata("A filmed stand-up special");
        m.fields
            .insert("tags".into(), "classification:ready, format:concert".into());
        assert_eq!(
            local_decision("format:stand-up", &m).verdict,
            Verdict::NoMatch
        );
        assert_eq!(local_decision("format:concert", &m).verdict, Verdict::Match);
        assert_eq!(local_decision("standup", &m).verdict, Verdict::Match);
    }
    #[test]
    fn comedy_does_not_prove_a_performance() {
        assert!(!classify(
            &metadata("A fictional story about an aspiring comedian"),
            vec![]
        )
        .labels
        .iter()
        .any(|l| l.dimension == "format"));
        assert_eq!(
            local_decision(
                "format:stand-up",
                &metadata("A fictional story about an aspiring comedian")
            )
            .verdict,
            Verdict::NoMatch
        );
    }
    #[test]
    fn explicit_performance_matches_without_a_provider() {
        assert_eq!(
            local_decision("format:stand-up", &metadata("A filmed stand-up special")).verdict,
            Verdict::Match
        );
    }
    #[test]
    fn exclusions_and_quotes_are_real_rules() {
        assert_eq!(
            local_decision(
                "\"stand-up special\" -backstage",
                &metadata("A backstage documentary about a stand-up special")
            )
            .verdict,
            Verdict::NoMatch
        );
    }
    #[test]
    fn metadata_keywords_supply_reusable_format_labels() {
        let c = classify(&metadata(""), vec!["stand-up comedy".into()]);
        assert!(c
            .terms(&Overrides::default())
            .contains(&"format:stand-up".into()));
        let o = Overrides {
            exclude: vec!["format:stand-up".into()],
            ..Default::default()
        };
        assert!(!c.terms(&o).contains(&"format:stand-up".into()));
    }
}
