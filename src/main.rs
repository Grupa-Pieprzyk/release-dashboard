#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![deny(rust_2018_idioms)]

use std::{collections::BTreeMap, io::Write};

use chrono::Local;
use itertools::Itertools;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};

use anyhow::{Context, Result};
use clap::Parser;
use futures::{Stream, StreamExt, TryFutureExt, TryStreamExt};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
struct SentryClient {
    client: reqwest::Client,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
struct Organization {
    pub id: String,
    pub slug: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[serde(untagged)]
enum EventTagKey {
    Key,
    Other(String),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct EventTag {
    pub key: EventTagKey,
    pub value: String,
}

// type EventTagMap = BTreeMap<EventTagKey, String>;
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EventTagMap {
    hostname: Option<String>,
    key: Option<String>,
    #[serde(flatten)]
    other: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct EventUser {
    username: Option<String>,
    ip_address: Option<String>,
}

// type Event = serde_json::Value;
#[derive(Serialize, Deserialize, Debug, Clone)]
struct Event {
    #[serde(rename = "eventID")]
    pub event_id: String,
    pub title: String,
    pub tags: Vec<EventTag>,
    // pub user: EventUser,
}

// type EventFull = serde_json::Value;
#[derive(Serialize, Deserialize, Debug, Clone)]
struct EventFull {
    pub id: String,
    pub title: String,
    pub tags: Vec<EventTag>,
    pub user: Option<EventUser>,
    pub contexts: BTreeMap<String, EventTagMap>,
    #[serde(rename = "dateCreated")]
    pub date_created: chrono::DateTime<Local>,
    // pub contexts: serde_json::Value,
}

// type Event = serde_json::Value;
#[derive(Serialize, Deserialize, Debug, Clone)]
struct Project {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub organization: Organization,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Issue {
    pub id: String,
}

macro_rules! get_url {
    ($self:expr, $endpoint:expr) => {
        Ok($self
            .client
            .get(SentryClient::endpoint($endpoint))
            .send()
            .await?
            .error_for_status()
            .context("bad status")?
            .json()
            .await?)
    };
}

const BASE_URL: &str = "https://sentry.io/api/0";
impl SentryClient {
    fn endpoint(uri: &str) -> String {
        format!("{}{}", BASE_URL, uri)
    }
    pub fn new(api_key: &str) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}")).context("bad header")?,
        );
        Ok(Self {
            client: reqwest::ClientBuilder::new()
                .default_headers(headers)
                .build()
                .context("creating client")?,
        })
    }

    pub async fn projects(&self) -> Result<Vec<Project>> {
        get_url!(self, "/projects/")
    }

    pub async fn project(&self, organization_slug: &str, project_slug: &str) -> Result<Project> {
        get_url!(
            self,
            &format!("/projects/{organization_slug}/{project_slug}/")
        )
    }
    pub async fn project_events_page(
        &self,
        organization_slug: &str,
        project_slug: &str,
        page: usize,
    ) -> Result<Vec<Event>> {
        const PAGE_SIZE: u64 = 100;
        let cursor = PAGE_SIZE * (page as u64);
        let res = get_url!(
            self,
            &format!("/projects/{organization_slug}/{project_slug}/events/?&cursor=0:{cursor}:0",)
        );
        log::debug!(
            "fetched {} items",
            res.as_ref().map(std::vec::Vec::len).unwrap_or(0)
        );
        res
    }

    pub fn project_events<'event, 'args: 'event, 'client: 'args>(
        &'client self,
        organization_slug: &'args str,
        project_slug: &'args str,
    ) -> impl Stream<Item = Result<Event>> + 'event {
        futures::stream::iter(0..)
            .then(move |page| self.project_events_page(organization_slug, project_slug, page))
            .take_while(|v| {
                futures::future::ready(v.as_ref().map(|v| !v.is_empty()).unwrap_or_default())
            })
            .map(|v| match v {
                Ok(v) => futures::stream::iter(v).map(Ok).left_stream(),
                Err(e) => futures::stream::iter([Err(e)]).right_stream(),
            })
            .flatten()
    }

    pub async fn event_full(
        &self,
        organization_slug: &str,
        project_slug: &str,
        event_id: String,
    ) -> Result<EventFull> {
        get_url!(
            self,
            &format!("/projects/{organization_slug}/{project_slug}/events/{event_id}/",)
        )
    }
}

/// Monitors cdkeys aggregated with various data of user
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// list organizations and exit
    #[clap(short, long)]
    list_organizations: bool,
    /// project slug
    #[clap(short, long)]
    project_slug: String,
    /// organization slug
    #[clap(short, long)]
    organization_slug: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct EventDetail {
    pub hostname: String,
    pub username: String,
    pub cd_key: String,
    pub title: String,
    pub ip_address: String,
    pub date_created: chrono::DateTime<Local>,
}

impl From<EventFull> for EventDetail {
    fn from(val: EventFull) -> Self {
        let EventFull {
            title,
            user,
            contexts,
            date_created,
            ..
        } = val;

        Self {
            hostname: contexts
                .iter()
                .find_map(|(_, v)| v.hostname.as_ref().cloned())
                .unwrap_or_else(|| "-".to_string()),
            username: user
                .as_ref()
                .and_then(|user| user.username.clone())
                .unwrap_or_else(|| "-".to_string()),
            ip_address: user
                .and_then(|user| user.ip_address)
                .unwrap_or_else(|| "-".to_string()),
            cd_key: contexts
                .iter()
                .find_map(|(_, v)| v.key.as_ref().cloned())
                .unwrap_or_else(|| "-".to_string()),
            date_created,
            title,
        }
    }
}

async fn extract_detail(event_full: EventFull) -> Result<EventDetail> {
    Ok(tokio::task::spawn_blocking(|| event_full.into()).await?)
}

macro_rules! summary {
    ($events:expr, $key_fn:expr, $title:expr) => {{
        {
            use colored::*;
            println!("\n\n ::: {} :::", $title.yellow());
            $events
                .into_iter()
                .sorted_by_key($key_fn)
                .group_by($key_fn)
                .into_iter()
                .for_each(|(key, e)| {
                    println!("{}", key.green());
                    for (date_created, msg) in e
                        .into_iter()
                        .sorted_by_key(|e| e.date_created.clone())
                        .map(
                            |EventDetail {
                                 hostname,
                                 username,
                                 cd_key,
                                 title,
                                 ip_address,
                                 date_created,
                             }| {


                                let mut username = username.clone();
                                for gowno in ['\u{200e}', '\u{200f}'] {
                                    username = username.replace(gowno, "_");
                                }
                                // println!("{:#?}", username);
                                let username = username.red();
                                let hostname = hostname.red();
                                let cd_key = cd_key.blue();
                                let _title = title.purple();
                                let ip_address = ip_address.magenta();
                                (
                                    date_created,
                                    format!("{username:55} {hostname:16} {ip_address:16} {cd_key:25}"),
                                )
                            },
                        )
                        .unique_by(|(_date_created, v)| v.clone())
                    {
                        let date_created = date_created.to_string().cyan().underline();
                        println!("   {date_created:20} {msg}")
                    }
                });
        }
    }};
}

async fn get_detail<'event, 'args: 'event, 'client: 'args>(
    client: &'client SentryClient,
    organization_slug: &'args str,
    project_slug: &'args str,
    event: Result<Event>,
) -> Result<EventDetail> {
    match event {
        Ok(event) => {
            client
                .event_full(organization_slug, project_slug, event.event_id)
                .and_then(extract_detail)
                .await
        }
        Err(e) => Err(e),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();
    pretty_env_logger::init();
    let args = Args::parse();
    let api_key = std::env::var("SENTRY_API_KEY").expect("api key required");
    let client = SentryClient::new(&api_key)?;
    if args.list_organizations {
        println!("{:#?}", client.projects().await?);
        return Ok(());
    }
    let mut stdout = std::io::stdout();

    println!();
    let project = client
        .project(&args.organization_slug, &args.project_slug)
        .await?;
    let events: Vec<EventDetail> = client
        .project_events(&project.organization.slug, &project.slug)
        .map(|event| get_detail(&client, &args.organization_slug, &args.project_slug, event))
        .buffer_unordered(30)
        .enumerate()
        .inspect(|(i, _)| {
            write!(stdout, "\rfetching {}", i).ok();
            stdout.flush().ok();
        })
        .map(|(_, v)| v)
        .try_collect()
        .await
        .context("collecting details")?;

    summary!(&events, |e| &e.ip_address, "by ip address");
    summary!(&events, |e| &e.username, "by username");
    summary!(&events, |e| &e.cd_key, "by cd_key");
    summary!(
        &events,
        |e| e.date_created.to_string().split_at(13).0.to_string(),
        "by hour"
    );
    summary!(&events, |e| &e.hostname, "by hostname");

    Ok(())
}
