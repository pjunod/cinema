//! Parse decoded client browse parameters without silently discarding paging.
use crate::identity::WireId;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    SortName,
    Name,
    Added,
    Year,
    PremiereDate,
    Index,
    ParentIndex,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowseQuery {
    pub start: i64,
    pub limit: i64,
    pub parent: Option<WireId>,
    pub user: Option<WireId>,
    pub series: Option<WireId>,
    pub season: Option<WireId>,
    pub recursive: bool,
    pub descending: bool,
    pub sorts: Vec<Sort>,
    pub kinds: Vec<String>,
    pub search: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid or oversized catalog query")]
pub struct InvalidQuery;
impl BrowseQuery {
    pub fn parse(pairs: &[(&str, &str)]) -> Result<Self, InvalidQuery> {
        if pairs.len() > 64 || pairs.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() > 8192 {
            return Err(InvalidQuery);
        }
        let mut result = Self {
            start: 0,
            limit: 100,
            parent: None,
            user: None,
            series: None,
            season: None,
            recursive: false,
            descending: false,
            sorts: vec![Sort::SortName],
            kinds: vec![],
            search: None,
        };
        let mut seen = std::collections::BTreeMap::<String, &str>::new();
        for &(key, value) in pairs {
            let key = key.to_ascii_lowercase();
            // A caller's token is parsed independently, with duplicate carriers preserved.
            if !matches!(
                key.as_str(),
                "startindex"
                    | "limit"
                    | "parentid"
                    | "userid"
                    | "seriesid"
                    | "seasonid"
                    | "recursive"
                    | "sortorder"
                    | "sortby"
                    | "includeitemtypes"
                    | "searchterm"
            ) {
                continue;
            }
            if let Some(prior) = seen.insert(key.clone(), value) {
                if prior != value {
                    return Err(InvalidQuery);
                }
                continue;
            }
            match key.as_str() {
                "startindex" => {
                    result.start = value.parse::<i64>().map_err(|_| InvalidQuery)?;
                    if result.start < 0 {
                        return Err(InvalidQuery);
                    }
                }
                "limit" => {
                    result.limit = value.parse::<i64>().map_err(|_| InvalidQuery)?;
                    if !(0..=500).contains(&result.limit) {
                        return Err(InvalidQuery);
                    }
                }
                "parentid" => result.parent = Some(WireId::parse(value).map_err(|_| InvalidQuery)?),
                "userid" => result.user = Some(WireId::parse(value).map_err(|_| InvalidQuery)?),
                "seriesid" => result.series = Some(WireId::parse(value).map_err(|_| InvalidQuery)?),
                "seasonid" => result.season = Some(WireId::parse(value).map_err(|_| InvalidQuery)?),
                "recursive" => {
                    result.recursive = match value.to_ascii_lowercase().as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return Err(InvalidQuery),
                    };
                }
                "sortorder" => {
                    result.descending = match value.to_ascii_lowercase().as_str() {
                        "ascending" => false,
                        "descending" => true,
                        _ => return Err(InvalidQuery),
                    };
                }
                "sortby" => {
                    result.sorts = value
                        .split(',')
                        .map(|name| match name.to_ascii_lowercase().as_str() {
                            "sortname" => Ok(Sort::SortName),
                            "name" => Ok(Sort::Name),
                            "datecreated" | "datelastcontentadded" | "datelastmediaadded" => {
                                Ok(Sort::Added)
                            }
                            "productionyear" => Ok(Sort::Year),
                            "premieredate" => Ok(Sort::PremiereDate),
                            "indexnumber" => Ok(Sort::Index),
                            "parentindexnumber" => Ok(Sort::ParentIndex),
                            _ => Err(InvalidQuery),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if result.sorts.len() > 4 {
                        return Err(InvalidQuery);
                    }
                }
                "includeitemtypes" => {
                    result.kinds = value
                        .split(',')
                        .map(|name| match name.to_ascii_lowercase().as_str() {
                            "movie" => Ok("movie".into()),
                            "series" => Ok("show".into()),
                            "season" => Ok("season".into()),
                            "episode" => Ok("episode".into()),
                            "collectionfolder" => Ok("library".into()),
                            _ => Err(InvalidQuery),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if result.kinds.len() > 5 {
                        return Err(InvalidQuery);
                    }
                }
                "searchterm" => {
                    if value.len() > 256 || value.chars().any(char::is_control) {
                        return Err(InvalidQuery);
                    }
                    result.search = Some(value.into());
                }
                _ => unreachable!("validated known key"),
            }
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browse_query_preserves_real_offsets_and_refuses_conflicting_aliases() {
        let q = BrowseQuery::parse(&[
            ("startIndex", "2400"),
            ("Limit", "100"),
            ("Recursive", "true"),
            ("IncludeItemTypes", "Movie,Episode"),
            ("SortBy", "SortName,ProductionYear"),
            ("sortOrder", "Descending"),
        ])
        .expect("target page");
        assert_eq!((q.start, q.limit), (2400, 100));
        assert!(q.recursive && q.descending);
        assert_eq!(q.kinds, ["movie", "episode"]);
        assert_eq!(q.sorts, [Sort::SortName, Sort::Year]);
        for pairs in [
            vec![("Limit", "100"), ("limit", "200")],
            vec![("startIndex", "-1")],
            vec![("Limit", "501")],
            vec![("sortBy", "unsupported")],
            vec![("parentId", "1")],
        ] {
            assert!(BrowseQuery::parse(&pairs).is_err());
        }
    }
}
