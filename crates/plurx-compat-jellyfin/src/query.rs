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
    /// `IncludeItemTypes` named only kinds Jellyfin defines but the Plurx
    /// catalogue never holds (BoxSet, Video, MusicVideo, ...). Jellyfin answers
    /// such a request with an empty page, so the facade does too.
    pub kinds_absent: bool,
    pub search: Option<String>,
}

/// Every `BaseItemKind` Jellyfin 10.11 accepts in `IncludeItemTypes`. A name
/// outside this list is a malformed request (Jellyfin refuses it as well); a
/// name inside it that Plurx does not catalogue selects nothing.
const JELLYFIN_ITEM_KINDS: &[&str] = &[
    "aggregatefolder",
    "audio",
    "audiobook",
    "basepluginfolder",
    "book",
    "boxset",
    "channel",
    "channelfolderitem",
    "collectionfolder",
    "episode",
    "folder",
    "genre",
    "manualplaylistsfolder",
    "movie",
    "livetvchannel",
    "livetvprogram",
    "musicalbum",
    "musicartist",
    "musicgenre",
    "musicvideo",
    "person",
    "photo",
    "photoalbum",
    "playlist",
    "playlistsfolder",
    "program",
    "recording",
    "season",
    "series",
    "studio",
    "trailer",
    "tvchannel",
    "tvprogram",
    "userrootfolder",
    "userview",
    "video",
    "year",
];

/// Every `ItemSortBy` Jellyfin 10.11 accepts. Orders Plurx cannot compute
/// (Random, CommunityRating, Runtime, ...) are skipped rather than refused, so
/// a client's chosen order degrades to the remaining keys or to SortName.
const JELLYFIN_SORTS: &[&str] = &[
    "default",
    "airedepisodeorder",
    "album",
    "albumartist",
    "artist",
    "datecreated",
    "officialrating",
    "dateplayed",
    "premieredate",
    "startdate",
    "sortname",
    "name",
    "random",
    "runtime",
    "communityrating",
    "productionyear",
    "playcount",
    "criticrating",
    "isfolder",
    "isunplayed",
    "isplayed",
    "seriessortname",
    "videobitrate",
    "airtime",
    "studio",
    "isfavoriteorliked",
    "datelastcontentadded",
    "datelastmediaadded",
    "seriesdateplayed",
    "parentindexnumber",
    "indexnumber",
];
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
            // Jellyfin answers a request without `Limit` with every row.
            limit: i64::MAX,
            parent: None,
            user: None,
            series: None,
            season: None,
            recursive: false,
            descending: false,
            sorts: vec![Sort::SortName],
            kinds: vec![],
            kinds_absent: false,
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
                    if result.limit < 0 {
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
                    let mut sorts = Vec::new();
                    for name in value.split(',') {
                        let name = name.trim().to_ascii_lowercase();
                        let sort = match name.as_str() {
                            "" => continue,
                            "sortname" => Sort::SortName,
                            "name" => Sort::Name,
                            "datecreated" | "datelastcontentadded" | "datelastmediaadded" => {
                                Sort::Added
                            }
                            "productionyear" => Sort::Year,
                            "premieredate" => Sort::PremiereDate,
                            "indexnumber" => Sort::Index,
                            "parentindexnumber" => Sort::ParentIndex,
                            other if JELLYFIN_SORTS.contains(&other) => continue,
                            _ => return Err(InvalidQuery),
                        };
                        if !sorts.contains(&sort) && sorts.len() < 4 {
                            sorts.push(sort);
                        }
                    }
                    result.sorts = if sorts.is_empty() {
                        vec![Sort::SortName]
                    } else {
                        sorts
                    };
                }
                "includeitemtypes" => {
                    let mut named = false;
                    let mut kinds = Vec::<String>::new();
                    for name in value.split(',') {
                        let name = name.trim().to_ascii_lowercase();
                        let kind = match name.as_str() {
                            "" => continue,
                            "movie" => "movie",
                            "series" => "show",
                            "season" => "season",
                            "episode" => "episode",
                            "collectionfolder" => "library",
                            other if JELLYFIN_ITEM_KINDS.contains(&other) => {
                                named = true;
                                continue;
                            }
                            _ => return Err(InvalidQuery),
                        };
                        named = true;
                        if !kinds.iter().any(|k| k == kind) {
                            kinds.push(kind.into());
                        }
                    }
                    result.kinds_absent = named && kinds.is_empty();
                    result.kinds = kinds;
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
            vec![("Limit", "-1")],
            vec![("sortBy", "unsupported")],
            vec![("IncludeItemTypes", "NotAJellyfinKind")],
            vec![("parentId", "1")],
        ] {
            assert!(BrowseQuery::parse(&pairs).is_err());
        }
    }

    /// Infuse and the Jellyfin apps ask for every kind and order Jellyfin
    /// defines. A valid name Plurx has no rows or order for must narrow or
    /// degrade the page, never turn it into a 400.
    #[test]
    fn browse_query_accepts_every_jellyfin_kind_and_order() {
        let q = BrowseQuery::parse(&[("IncludeItemTypes", "BoxSet,Video,Folder")]).expect("kinds");
        assert!(q.kinds.is_empty() && q.kinds_absent, "{q:?}");
        let q = BrowseQuery::parse(&[("IncludeItemTypes", "Movie,BoxSet,movie")]).expect("mixed");
        assert_eq!(q.kinds, ["movie"]);
        assert!(!q.kinds_absent);
        let q = BrowseQuery::parse(&[("IncludeItemTypes", "CollectionFolder")]).expect("views");
        assert_eq!(q.kinds, ["library"]);
        let q = BrowseQuery::parse(&[]).expect("default");
        assert!(q.kinds.is_empty() && !q.kinds_absent);
        assert_eq!(q.limit, i64::MAX, "no Limit means every row");
        let q = BrowseQuery::parse(&[("Limit", "2000")]).expect("large page");
        assert_eq!(q.limit, 2000);
        let q = BrowseQuery::parse(&[("SortBy", "Random,CommunityRating")]).expect("orders");
        assert_eq!(q.sorts, [Sort::SortName]);
        let q = BrowseQuery::parse(&[(
            "SortBy",
            "IsFolder,SortName,Runtime,ProductionYear,SortName",
        )])
        .expect("mixed orders");
        assert_eq!(q.sorts, [Sort::SortName, Sort::Year]);
        for name in JELLYFIN_ITEM_KINDS {
            assert!(
                BrowseQuery::parse(&[("includeItemTypes", name)]).is_ok(),
                "{name}"
            );
        }
        for name in JELLYFIN_SORTS {
            assert!(BrowseQuery::parse(&[("sortBy", name)]).is_ok(), "{name}");
        }
    }
}
