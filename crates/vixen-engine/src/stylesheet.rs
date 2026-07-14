use crate::doc::DocumentStyleItem;
use crate::page::Page;

pub(crate) struct PageStylesheetRunner {
    items: std::vec::IntoIter<DocumentStyleItem>,
    csp: vixen_net::csp::ContentSecurityPolicy,
    origin: vixen_net::Origin,
    context_trustworthy: bool,
}

pub(crate) enum PreparedPageStylesheet {
    Skip,
    External(ExternalPageStylesheet),
}

#[derive(Clone)]
pub(crate) struct ExternalPageStylesheet {
    index: usize,
    url: url::Url,
    csp: vixen_net::csp::ContentSecurityPolicy,
    origin: vixen_net::Origin,
    context_trustworthy: bool,
}

impl PageStylesheetRunner {
    pub(crate) fn new(page: &Page) -> Self {
        let document_url = url::Url::parse(page.url()).ok();
        Self {
            items: page.document().style_execution_items().into_iter(),
            csp: page.csp().clone(),
            origin: document_url
                .as_ref()
                .map(vixen_net::Origin::from_url)
                .unwrap_or_else(vixen_net::Origin::opaque),
            context_trustworthy: document_url
                .as_ref()
                .is_some_and(vixen_net::referrer_policy::is_potentially_trustworthy),
        }
    }

    pub(crate) fn prepare_next(&mut self, page: &Page) -> Option<PreparedPageStylesheet> {
        loop {
            let item = self.items.next()?;
            match item {
                DocumentStyleItem::CspMeta(policy) => {
                    self.csp.add_header(&policy);
                    return Some(PreparedPageStylesheet::Skip);
                }
                DocumentStyleItem::InlineStyle(_) => continue,
                DocumentStyleItem::ExternalStylesheet { index, href } => {
                    let Some(url) = page
                        .resolve_url(&href)
                        .and_then(|resolved| url::Url::parse(&resolved).ok())
                    else {
                        return Some(PreparedPageStylesheet::Skip);
                    };
                    let request = ExternalPageStylesheet {
                        index,
                        url,
                        csp: self.csp.clone(),
                        origin: self.origin.clone(),
                        context_trustworthy: self.context_trustworthy,
                    };
                    return Some(if request.allows_url(request.url()) {
                        PreparedPageStylesheet::External(request)
                    } else {
                        PreparedPageStylesheet::Skip
                    });
                }
            }
        }
    }
}

impl ExternalPageStylesheet {
    pub(crate) fn index(&self) -> usize {
        self.index
    }

    pub(crate) fn url(&self) -> &url::Url {
        &self.url
    }

    pub(crate) fn allows_url(&self, url: &url::Url) -> bool {
        self.blocked_reason(url).is_none()
    }

    pub(crate) fn blocked_reason(&self, url: &url::Url) -> Option<&'static str> {
        if !self.csp.allows_fetch("style-src", url, &self.origin) {
            return Some("csp");
        }
        if matches!(
            vixen_net::classify_mixed_content(
                self.context_trustworthy,
                url,
                vixen_net::ResourceType::Stylesheet,
                false,
            ),
            vixen_net::MixedContentVerdict::Block
        ) {
            return Some("mixed-content");
        }
        None
    }

    pub(crate) fn is_cross_site(&self, url: &url::Url) -> bool {
        !vixen_net::is_same_site(&self.origin, &vixen_net::Origin::from_url(url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_src_is_rechecked_for_redirect_targets() {
        let page = Page::from_html_with_headers(
            "https://document.test/page",
            "<link rel='stylesheet' href='https://allowed.test/style.css'>",
            [("content-security-policy", "style-src https://allowed.test")],
        )
        .unwrap();
        let mut runner = PageStylesheetRunner::new(&page);
        let Some(PreparedPageStylesheet::External(request)) = runner.prepare_next(&page) else {
            panic!("allowed stylesheet was not prepared");
        };

        assert!(request.allows_url(request.url()));
        assert_eq!(
            request.blocked_reason(&url::Url::parse("https://blocked.test/style.css").unwrap()),
            Some("csp")
        );
    }

    #[test]
    fn active_mixed_content_stylesheet_is_skipped() {
        let page = Page::from_html(
            "https://document.test/page",
            "<link rel='stylesheet' href='http://document.test/style.css'>",
        )
        .unwrap();
        let mut runner = PageStylesheetRunner::new(&page);

        assert!(matches!(
            runner.prepare_next(&page),
            Some(PreparedPageStylesheet::Skip)
        ));
    }
}
