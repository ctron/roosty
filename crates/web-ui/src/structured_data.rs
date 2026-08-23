//! Typed, script-safe JSON-LD for public profile and status documents.

use serde::Serialize;

use crate::public_pages::{UiMediaKind, UiProfileHeader, UiStatusThread, UiStatusVisibility};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfilePage<'a> {
    #[serde(rename = "@context")]
    context: &'static str,
    #[serde(rename = "@type")]
    schema_type: &'static str,
    #[serde(rename = "@id")]
    id: String,
    url: &'a str,
    date_created: &'a str,
    main_entity: Person<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Person<'a> {
    #[serde(rename = "@type")]
    schema_type: &'static str,
    #[serde(rename = "@id")]
    id: String,
    url: &'a str,
    name: &'a str,
    alternate_name: String,
    identifier: String,
    interaction_statistic: Vec<InteractionCounter>,
    agent_interaction_statistic: Vec<InteractionCounter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InteractionCounter {
    #[serde(rename = "@type")]
    schema_type: &'static str,
    interaction_type: InteractionType,
    user_interaction_count: u64,
}

#[derive(Serialize)]
struct InteractionType {
    #[serde(rename = "@type")]
    schema_type: &'static str,
}

impl InteractionCounter {
    fn new(schema_type: &'static str, count: u64) -> Self {
        Self {
            schema_type: "InteractionCounter",
            interaction_type: InteractionType { schema_type },
            user_interaction_count: count,
        }
    }
}

pub(crate) fn profile_structured_data(header: &UiProfileHeader) -> String {
    let account = &header.account;
    let name = if account.display_name.trim().is_empty() {
        account.username.as_str()
    } else {
        account.display_name.as_str()
    };
    script_safe_json(&ProfilePage {
        context: "https://schema.org",
        schema_type: "ProfilePage",
        id: format!("{}#profile-page", header.profile_url),
        url: &header.profile_url,
        date_created: &account.created_at,
        main_entity: Person {
            schema_type: "Person",
            id: format!("{}#person", header.profile_url),
            url: &header.profile_url,
            name,
            alternate_name: format!("@{}", account.username),
            identifier: account.id.to_string(),
            interaction_statistic: vec![InteractionCounter::new(
                "FollowAction",
                account.followers_count,
            )],
            agent_interaction_statistic: vec![
                InteractionCounter::new("FollowAction", account.following_count),
                InteractionCounter::new("WriteAction", account.statuses_count),
            ],
            description: (!account.bio.trim().is_empty()).then_some(account.bio.as_str()),
            image: account.avatar_url.as_deref(),
        },
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SocialMediaPosting<'a> {
    #[serde(rename = "@context")]
    context: &'static str,
    #[serde(rename = "@type")]
    schema_type: &'static str,
    #[serde(rename = "@id")]
    id: String,
    url: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    article_body: &'a str,
    author: Author<'a>,
    date_published: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    date_modified: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    image: Vec<ImageObject<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    video: Vec<VideoObject<'a>>,
    comment_count: u64,
    interaction_statistic: Vec<InteractionCounter>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    comment: Vec<Comment<'a>>,
}

#[derive(Serialize)]
struct Author<'a> {
    #[serde(rename = "@type")]
    schema_type: &'static str,
    name: &'a str,
    url: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImageObject<'a> {
    #[serde(rename = "@type")]
    schema_type: &'static str,
    content_url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    caption: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VideoObject<'a> {
    #[serde(rename = "@type")]
    schema_type: &'static str,
    name: &'a str,
    description: &'a str,
    content_url: &'a str,
    thumbnail_url: &'a str,
    upload_date: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Comment<'a> {
    #[serde(rename = "@type")]
    schema_type: &'static str,
    url: &'a str,
    text: &'a str,
    date_created: &'a str,
    author: Author<'a>,
}

pub(crate) fn posting_structured_data(thread: &UiStatusThread) -> Option<String> {
    let status = &thread.status;
    if status.sensitive {
        return None;
    }
    let image = status
        .media
        .iter()
        .filter(|media| matches!(media.kind, UiMediaKind::Image))
        .map(|media| ImageObject {
            schema_type: "ImageObject",
            content_url: &media.url,
            caption: media.description.as_deref(),
            width: media.width,
            height: media.height,
        })
        .collect::<Vec<_>>();
    let video = status
        .media
        .iter()
        .filter_map(|media| {
            if !matches!(media.kind, UiMediaKind::Video) {
                return None;
            }
            let thumbnail_url = media.preview_url.as_deref()?;
            let description = media.description.as_deref()?.trim();
            (!description.is_empty()).then_some(VideoObject {
                schema_type: "VideoObject",
                name: description,
                description,
                content_url: &media.url,
                thumbnail_url,
                upload_date: &status.created_at,
            })
        })
        .collect::<Vec<_>>();
    if status.content_text.trim().is_empty() && image.is_empty() && video.is_empty() {
        return None;
    }
    let comment = thread
        .descendants
        .iter()
        .filter(|comment| {
            !comment.sensitive
                && !comment.content_text.trim().is_empty()
                && matches!(
                    comment.visibility,
                    UiStatusVisibility::Public | UiStatusVisibility::Unlisted
                )
        })
        .map(|comment| Comment {
            schema_type: "Comment",
            url: &comment.url,
            text: &comment.content_text,
            date_created: &comment.created_at,
            author: Author {
                schema_type: "Person",
                name: &comment.author.display_name,
                url: &comment.author.url,
            },
        })
        .collect();
    Some(script_safe_json(&SocialMediaPosting {
        context: "https://schema.org",
        schema_type: "SocialMediaPosting",
        id: format!("{}#posting", thread.canonical_url),
        url: &thread.canonical_url,
        article_body: &status.content_text,
        author: Author {
            schema_type: "Person",
            name: &status.author.display_name,
            url: &status.author.url,
        },
        date_published: &status.created_at,
        date_modified: status.edited_at.as_deref(),
        image,
        video,
        comment_count: status.replies_count,
        interaction_statistic: vec![
            InteractionCounter::new("LikeAction", status.favourites_count),
            InteractionCounter::new("ShareAction", status.reblogs_count),
            InteractionCounter::new("ReplyAction", status.replies_count),
        ],
        comment,
    }))
}

fn script_safe_json(value: &impl Serialize) -> String {
    serde_json::to_string(value)
        .expect("serializing typed structured data cannot fail")
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use uuid::Uuid;

    use super::{posting_structured_data, profile_structured_data};
    use crate::public_pages::{
        UiMedia, UiMediaKind, UiProfileHeader, UiPublicAccount, UiStatus, UiStatusAuthor,
        UiStatusThread, UiStatusVisibility,
    };

    fn status(text: &str) -> UiStatus {
        let id = Uuid::nil();
        UiStatus {
            id,
            author: UiStatusAuthor {
                display_name: "Alice".to_owned(),
                handle: "@alice".to_owned(),
                url: "https://roosty.test/@alice".to_owned(),
                avatar_url: None,
                local: true,
            },
            url: format!("https://roosty.test/@alice/{id}"),
            activitypub_url: format!("https://roosty.test/users/alice/statuses/{id}"),
            content_html: format!("<p>{text}</p>"),
            content_text: text.to_owned(),
            spoiler_text: String::new(),
            sensitive: false,
            visibility: UiStatusVisibility::Public,
            created_at: "2026-08-23T10:00:00Z".to_owned(),
            edited_at: None,
            media: Vec::new(),
            poll: None,
            card: None,
            quote: None,
            replies_count: 3,
            reblogs_count: 2,
            favourites_count: 1,
            pinned: false,
        }
    }

    fn account() -> UiPublicAccount {
        UiPublicAccount {
            id: Uuid::nil(),
            username: "alice".to_owned(),
            display_name: "Alice </script>".to_owned(),
            bio: "Hello & welcome".to_owned(),
            avatar_url: None,
            header_url: None,
            fields: Vec::new(),
            created_at: "2026-08-23T10:00:00Z".to_owned(),
            followers_count: 1,
            following_count: 2,
            statuses_count: 3,
            limited: false,
            discoverable: true,
        }
    }

    fn thread(status: UiStatus) -> UiStatusThread {
        UiStatusThread {
            account: account(),
            ancestors: Vec::new(),
            canonical_url: status.url.clone(),
            activitypub_url: status.activitypub_url.clone(),
            status,
            descendants: Vec::new(),
            noindex: false,
            search_indexing_enabled: true,
        }
    }

    #[test]
    fn profile_uses_stable_ids_and_classified_counter_arrays_and_is_script_safe() {
        let data = profile_structured_data(&UiProfileHeader {
            account: account(),
            featured_tags: Vec::new(),
            profile_url: "https://roosty.test/@alice".to_owned(),
            activitypub_url: "https://roosty.test/users/alice".to_owned(),
            search_indexing_enabled: true,
        });
        assert!(!data.contains("</script>"));
        let data: Value = serde_json::from_str(&data).unwrap();
        assert_eq!(data["@id"], "https://roosty.test/@alice#profile-page");
        assert_eq!(
            data["mainEntity"]["@id"],
            "https://roosty.test/@alice#person"
        );
        assert_eq!(
            data["mainEntity"]["interactionStatistic"][0]["interactionType"]["@type"],
            "FollowAction"
        );
        assert_eq!(
            data["mainEntity"]["agentInteractionStatistic"][1]["interactionType"]["@type"],
            "WriteAction"
        );
    }

    #[test]
    fn posting_includes_edits_ordered_comments_and_interaction_counters() {
        let mut focus = status("Hello <script>");
        focus.edited_at = Some("2026-08-23T11:00:00Z".to_owned());
        let mut thread = thread(focus);
        thread.descendants = vec![status("First reply"), status("Second reply")];
        thread.descendants[1].sensitive = true;
        let serialized = posting_structured_data(&thread).unwrap();
        assert!(!serialized.contains("<script>"));
        let data: Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(data["@type"], "SocialMediaPosting");
        assert_eq!(data["dateModified"], "2026-08-23T11:00:00Z");
        assert_eq!(data["comment"].as_array().unwrap().len(), 1);
        assert_eq!(data["comment"][0]["text"], "First reply");
        assert_eq!(data["interactionStatistic"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn posting_eligibility_handles_sensitive_and_media_only_statuses() {
        let mut sensitive = status("hidden");
        sensitive.sensitive = true;
        assert!(posting_structured_data(&thread(sensitive)).is_none());
        assert!(posting_structured_data(&thread(status(""))).is_none());

        let mut image = status("");
        image.media.push(UiMedia {
            kind: UiMediaKind::Image,
            content_type: Some("image/png".to_owned()),
            url: "https://roosty.test/media/image.png".to_owned(),
            preview_url: None,
            description: Some("A view".to_owned()),
            width: Some(800),
            height: Some(600),
        });
        assert!(posting_structured_data(&thread(image)).is_some());

        let mut incomplete_video = status("");
        incomplete_video.media.push(UiMedia {
            kind: UiMediaKind::Video,
            content_type: Some("video/mp4".to_owned()),
            url: "https://roosty.test/media/video.mp4".to_owned(),
            preview_url: None,
            description: None,
            width: None,
            height: None,
        });
        assert!(posting_structured_data(&thread(incomplete_video)).is_none());
    }
}
