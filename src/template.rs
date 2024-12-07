use std::error::Error;
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Datelike, FixedOffset};
use eyre::{bail, eyre, WrapErr};
use serde::{Deserialize, Serialize};
use tera::{Context, Tera};

use crate::entry::{AuthorMetadata, Entry};
use crate::error::Error as GempostError;
use crate::feed::{Feed, FeedAuthor};
use crate::page::Pages;
use crate::page_entry::PageEntry;

fn create_breadcrumb(file_path: &Path, base_path: &Path) -> Vec<String> {
    // Strip the first N components, which will give us the
    // breadcrumb within the context of the capsule itself,
    // instead of a full absolute path on local filesystem.
    let base_path = base_path.canonicalize().ok().unwrap_or_default();
    let num_base_components = base_path.components().count();

    let binding = file_path.canonicalize().ok().unwrap_or_default();
    let capsule_components = binding.components().skip(num_base_components);

    let mut breadcrumb: Vec<String> = capsule_components
        .flat_map(|c| match c {
            Component::Normal(breadcrumb) => Some(breadcrumb),
            _ => None,
        })
        .map(|comp_str| comp_str.to_string_lossy().into())
        .collect();

    // Remove the file extension from the last element in the
    // breadcrumb.
    if let Some(filename) = breadcrumb.last_mut() {
        if let Some(stem) = Path::new(filename).file_stem() {
            *filename = stem.to_string_lossy().into();
        }
    }

    breadcrumb
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct EntryAuthorTemplateData {
    pub name: String,
    pub email: Option<String>,
    pub uri: Option<String>,
}

impl From<AuthorMetadata> for EntryAuthorTemplateData {
    fn from(value: AuthorMetadata) -> Self {
        Self {
            name: value.name,
            email: value.email,
            uri: value.uri,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct EntryTemplateData {
    pub id: String,
    pub url: String,
    pub title: String,
    pub body: String,
    pub updated: DateTime<FixedOffset>,
    pub summary: Option<String>,
    pub published: Option<DateTime<FixedOffset>>,
    pub publish_year: Option<i32>,
    pub author: Option<EntryAuthorTemplateData>,
    pub rights: Option<String>,
    pub lang: Option<String>,
    pub categories: Vec<String>,
    pub layout: Option<String>,
    pub values: serde_yaml::Mapping,
}

impl From<Entry> for EntryTemplateData {
    fn from(params: Entry) -> Self {
        let published = params.metadata.published.clone();

        Self {
            id: params.metadata.id,
            url: params.url.to_string(),
            title: params.metadata.title,
            body: params.body,
            updated: params.metadata.updated.clone(),
            summary: params.metadata.summary,
            published,
            publish_year: published.map(|d| d.year()),
            author: params.metadata.author.map(Into::into),
            rights: params.metadata.rights,
            lang: params.metadata.lang,
            categories: params.metadata.categories,
            layout: params.metadata.layout,
            values: params.metadata.values,
        }
    }
}

impl From<PageEntry> for EntryTemplateData {
    fn from(params: PageEntry) -> Self {
        let published = params.metadata.published.clone();
        Self {
            id: params.metadata.id,
            url: params.url.to_string(),
            title: params.metadata.title,
            body: params.body,
            updated: params.metadata.updated.clone(),
            summary: params.metadata.summary,
            published,
            publish_year: published.map(|d| d.year()),
            author: params.metadata.author.map(Into::into),
            rights: params.metadata.rights,
            lang: params.metadata.lang,
            categories: params.metadata.categories,
            layout: params.metadata.layout,
            values: params.metadata.values,
        }
    }
}

fn create_named_template<P: AsRef<Path>>(path: &P) -> (&Path, Option<&str>) {
    let path = path.as_ref();
    (path, path.file_stem().and_then(|stem| stem.to_str()))
}

fn to_error(output: &Path, url: &str, err: tera::Error) -> eyre::Result<()> {
    let err_message = err.to_string();
    let src_message = err.source().map(|e| e.to_string());

    let mut errors: Vec<&dyn Error> = vec![&err];
    let mut curr_err = err.source();

    while let Some(error) = curr_err {
        errors.push(error);
        curr_err = error.source();
    }

    let message = errors
        .into_iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join(" - ");

    bail!(GempostError::InvalidPageTemplate {
        path: output.to_owned(),
        reason: format!("{}: {}", url, message)
    });
}

impl EntryTemplateData {
    pub fn render_post(
        &self,
        feed: &FeedTemplateData,
        template: &Path,
        output: &Path,
    ) -> eyre::Result<()> {
        let mut tera = Tera::default();

        if let Err(err) = tera.add_template_file(template, Some("post")) {
            bail!(GempostError::InvalidPostPageTemplate {
                path: output.to_owned(),
                reason: err.to_string(),
            });
        }

        let mut context = Context::new();
        context.insert("entry", self);
        context.insert("feed", feed);

        let dest_file = File::create(output).wrap_err("failed creating gemlog post page file")?;

        if let Err(err) = tera.render_to("post", &context, dest_file) {
            bail!(GempostError::InvalidPostPageTemplate {
                path: output.to_owned(),
                reason: err.to_string(),
            });
        }

        Ok(())
    }

    pub fn render_page<P: AsRef<Path>>(
        &mut self,
        pages_data: &PagesTemplateData,
        templates: &[P],
        output: &Path,
    ) -> eyre::Result<()> {
        let mut tera = Tera::default();
        let templates: Vec<_> = templates
            .into_iter()
            .map(|tmpl_path| create_named_template(tmpl_path))
            .collect();

        let page_template = templates
            .iter()
            .find(|(_, name)| name.unwrap_or_default() == "page");

        if let Some(_) = page_template {
            bail!(GempostError::InvalidPageTemplate {
                path: output.to_owned(),
                reason: "page template directory must NOT contain a page.tera file".to_string(),
            });
        }

        if let Err(err) = tera.add_template_files(templates) {
            bail!(GempostError::InvalidPageTemplate {
                path: output.to_owned(),
                reason: err.to_string(),
            });
        }

        if let Err(err) = tera.add_raw_template("page", &self.body) {
            bail!(GempostError::InvalidPageTemplate {
                path: output.to_owned(),
                reason: format!("{}: {}", self.url, err.to_string())
            });
        }

        // creation of the file must be first to make sure
        // canonicalize() works to create breadcrumb.
        let parent_dir = output.parent().ok_or(eyre!(
            "Could not get parent directory of templated page file. This is a bug."
        ))?;

        fs::create_dir_all(parent_dir).wrap_err("failed creating parent directory")?;

        let dest_file =
            File::create(output).wrap_err("failed creating gemlog templated page file")?;

        let breadcrumb = create_breadcrumb(output, &pages_data.pages_dir);

        let mut context = Context::new();
        context.insert("entry", self);
        context.insert("values", &self.values);
        context.insert("breadcrumb", &breadcrumb);
        context.insert("feed", &pages_data.feed_data);

        // render content itself, then render via layout.
        let pre_rendered = match tera.render("page", &context) {
            Ok(rendered) => rendered,
            Err(err) => return to_error(output, &self.url, err),
        };

        context.insert("content", &pre_rendered);

        let layout_template = self.layout.as_deref().unwrap_or("base");
        if let Err(err) = tera.render_to(layout_template, &context, dest_file) {
            return to_error(output, &self.url, err);
        }

        Ok(())
    }
}

impl FeedTemplateData {
    pub fn render_index(&self, template: &Path, output: &Path) -> eyre::Result<()> {
        let mut tera = Tera::default();

        if let Err(err) = tera.add_template_file(template, Some("index")) {
            bail!(GempostError::InvalidIndexPageTemplate {
                reason: err.to_string()
            });
        }

        let mut context = Context::new();
        context.insert("feed", self);

        let parent_dir = output.parent().ok_or(eyre!(
            "Could not get parent directory of index page file. This is a bug."
        ))?;

        fs::create_dir_all(parent_dir).wrap_err("failed creating parent directory")?;

        let dest_file = File::create(output).wrap_err("failed creating gemlog index page file")?;

        if let Err(err) = tera.render_to("index", &context, dest_file) {
            bail!(GempostError::InvalidIndexPageTemplate {
                reason: err.to_string(),
            });
        }

        Ok(())
    }

    pub fn render_feed(&self, template: &str, output: &Path) -> eyre::Result<()> {
        let mut tera = Tera::default();

        tera.add_raw_template("feed", template)
            .wrap_err("The bundled Atom feed template is invalid. This is a bug.")?;

        let mut context = Context::new();
        context.insert("feed", self);

        let parent_dir = output.parent().ok_or(eyre!(
            "Could not get parent directory of Atom feed file. This is a bug."
        ))?;

        fs::create_dir_all(parent_dir).wrap_err("failed creating parent directory")?;

        let dest_file = File::create(output).wrap_err("failed creating gemlog Atom feed file")?;

        tera.render_to("feed", &context, dest_file)
            .wrap_err("failed generating the Atom feed")?;

        Ok(())
    }
}

#[derive(Debug)]
pub struct PostPathParams {
    pub slug: String,
    pub published: Option<DateTime<chrono::FixedOffset>>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostPathTemplateData {
    pub year: String,
    pub month: String,
    pub day: String,
    pub slug: String,
}

impl From<PostPathParams> for PostPathTemplateData {
    fn from(params: PostPathParams) -> Self {
        Self {
            // If there is no publish date, these are empty strings.
            year: params
                .published
                .map(|published| format!("{:0>4}", published.year()))
                .unwrap_or_default(),
            month: params
                .published
                .map(|published| format!("{:0>2}", published.month()))
                .unwrap_or_default(),
            day: params
                .published
                .map(|published| format!("{:0>2}", published.day()))
                .unwrap_or_default(),
            slug: params.slug,
        }
    }
}

impl PostPathTemplateData {
    pub fn render(&self, template: &str) -> eyre::Result<String> {
        let mut tera = Tera::default();

        if let Err(err) = tera.add_raw_template("path", template) {
            bail!(GempostError::InvalidPostPath {
                template: template.to_owned(),
                reason: err.to_string(),
            });
        }

        let mut context = Context::new();
        context.insert("year", &self.year);
        context.insert("month", &self.month);
        context.insert("day", &self.day);
        context.insert("slug", &self.slug);

        match tera.render("path", &context) {
            Ok(path) => Ok(path),
            Err(err) => bail!(GempostError::InvalidPostPath {
                template: template.to_owned(),
                reason: err.to_string(),
            }),
        }
    }
}

pub struct PagePathParams<'a> {
    pub base_path: &'a Path,
    pub file_path: &'a Path,
    pub slug: &'a str,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PagePathTemplateData {
    pub parent_url: String,
    pub slug: String,
}

impl<'a> From<PagePathParams<'a>> for PagePathTemplateData {
    fn from(params: PagePathParams) -> Self {
        // Breadcrumb needs to drop the last entry, as it's the page
        // name in this case.
        let mut breadcrumb = create_breadcrumb(params.file_path, params.base_path);
        breadcrumb.pop();

        Self {
            parent_url: breadcrumb.join("/"),
            slug: params.slug.to_string(),
        }
    }
}

impl PagePathTemplateData {
    pub fn render(&self, template: &str) -> eyre::Result<String> {
        let mut tera = Tera::default();

        if let Err(err) = tera.add_raw_template("path", template) {
            bail!(GempostError::InvalidPostPath {
                template: template.to_owned(),
                reason: err.to_string(),
            });
        }

        let mut context = Context::new();
        context.insert("breadcrumb", &self.parent_url);
        context.insert("slug", &self.slug);

        match tera.render("path", &context) {
            Ok(path) => Ok(path),
            Err(err) => bail!(GempostError::InvalidPostPath {
                template: template.to_owned(),
                reason: err.to_string(),
            }),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct FeedAuthorTemplateData {
    pub name: String,
    pub email: Option<String>,
    pub uri: Option<String>,
}

impl From<FeedAuthor> for FeedAuthorTemplateData {
    fn from(value: FeedAuthor) -> Self {
        Self {
            name: value.name,
            email: value.email,
            uri: value.uri,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct FeedTemplateData {
    pub capsule_url: String,
    pub feed_url: String,
    pub index_url: String,
    pub title: String,
    pub updated: String,
    pub subtitle: Option<String>,
    pub rights: Option<String>,
    pub author: Option<FeedAuthorTemplateData>,
    pub entries: Vec<EntryTemplateData>,
}

impl From<Feed> for FeedTemplateData {
    fn from(feed: Feed) -> Self {
        Self {
            capsule_url: feed.capsule_url.to_string(),
            feed_url: feed.feed_url.to_string(),
            index_url: feed.index_url.to_string(),
            title: feed.title,
            updated: feed.updated.to_rfc3339(),
            subtitle: feed.subtitle,
            rights: feed.rights,
            author: feed.author.map(Into::into),
            entries: feed.entries.into_iter().map(Into::into).collect(),
        }
    }
}

pub struct PagesTemplateData {
    pub capsule_url: String,
    pub index_url: String,
    pub pages_dir: PathBuf,
    pub feed_data: FeedTemplateData,
}

impl From<Pages> for PagesTemplateData {
    fn from(pages: Pages) -> Self {
        Self {
            capsule_url: pages.capsule_url.to_string(),
            index_url: pages.index_url.to_string(),
            pages_dir: pages.pages_path,
            feed_data: FeedTemplateData::from(pages.feed),
        }
    }
}
