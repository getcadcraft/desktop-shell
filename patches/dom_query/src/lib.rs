// SPDX-License-Identifier: Apache-2.0
//! CadCraft's own stand-in for the `dom_query` crate (whose selector engine, servo's `selectors` + `cssparser`, is
//! MPL-2.0 and may not ship in the shell: docs/0.4-PART2.md L.2). Exactly the API its two users call:
//!
//! - **wry** on Android (shipped): puts initialization scripts first in `<head>` when the system WebView is too old for
//!   `addDocumentStartJavaScript`.
//! - **tauri-utils `html2`** (build time, every platform: tauri-codegen): adds a CSP `<meta>`, a script to `<head>`, nonce
//!   attributes on `style` / `script[src^='http']`, inlines `script[src]`.
//!
//! No DOM tree: the page is kept as text and its start tags are scanned once (comments, `<!…>`, end tags skipped; the
//! text of script/style/textarea/title is raw). Edits are recorded and applied when serialising, so every byte not edited
//! comes out as it went in. Selectors: a tag name and/or attribute tests (`[a]`, `[a=v]`, `[a^=v]`, quoted or not),
//! comma-separated; anything else selects nothing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;

const RAW_TEXT: [&str; 4] = ["script", "style", "textarea", "title"];

struct Tag {
    name: String,
    start: usize,
    /// Just after the start tag's `>`.
    end: usize,
    attrs: Vec<(String, Option<String>)>,
    /// Raw-text elements: where their text ends (the `</name`).
    text_end: Option<usize>,
}

#[derive(Default, Clone)]
struct Element {
    name: String,
    attrs: Vec<(String, String)>,
    text: Option<String>,
}

#[derive(Default)]
struct Inner {
    src: String,
    tags: Vec<Tag>,
    /// Edits to tags of the page: new attribute list, new text.
    edits: HashMap<usize, Element>,
    /// Elements made with new_element, by id.
    made: Vec<Element>,
    head_first: Vec<usize>,
    head_last: Vec<usize>,
    /// No <head> in the page: one is made (where the page's <html> starts, else at the start).
    made_head: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Page(usize),
    Made(usize),
    /// The head we make when the page has none.
    NewHead,
    /// The html root when the page has no <html> tag.
    NoHtml,
}

pub struct Document {
    pub tree: Tree,
    inner: Rc<RefCell<Inner>>,
}

pub struct Tree {
    inner: Rc<RefCell<Inner>>,
}

#[derive(Clone)]
pub struct NodeRef<'a> {
    kind: Kind,
    inner: Rc<RefCell<Inner>>,
    _doc: PhantomData<&'a ()>,
}

pub struct Selection<'a> {
    nodes: Vec<NodeRef<'a>>,
}

impl<'a> Selection<'a> {
    pub fn nodes(&self) -> &[NodeRef<'a>] {
        &self.nodes
    }
    pub fn length(&self) -> usize {
        self.nodes.len()
    }
    pub fn iter(&self) -> impl Iterator<Item = NodeRef<'a>> + '_ {
        self.nodes.iter().cloned()
    }
}

fn scan(src: &str) -> Vec<Tag> {
    let b = src.as_bytes();
    let lower = src.to_ascii_lowercase();
    let mut tags = Vec::new();
    let mut i = 0;
    while let Some(off) = src[i..].find('<') {
        let at = i + off;
        let rest = &src[at..];
        if rest.starts_with("<!--") {
            i = src[at + 4..].find("-->").map(|e| at + 4 + e + 3).unwrap_or(src.len());
            continue;
        }
        if rest.starts_with("</") || rest.starts_with("<!") || rest.starts_with("<?") {
            i = src[at..].find('>').map(|e| at + e + 1).unwrap_or(src.len());
            continue;
        }
        let mut j = at + 1;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'-' || b[j] == b':') {
            j += 1;
        }
        if j == at + 1 || !b[at + 1].is_ascii_alphabetic() {
            i = at + 1;
            continue;
        }
        let name = lower[at + 1..j].to_string();
        // Attributes up to the tag's '>' (quotes respected).
        let mut attrs = Vec::new();
        let mut k = j;
        let end = loop {
            while k < b.len() && (b[k].is_ascii_whitespace() || b[k] == b'/') {
                k += 1;
            }
            if k >= b.len() {
                break b.len();
            }
            if b[k] == b'>' {
                break k + 1;
            }
            let n0 = k;
            while k < b.len() && !b[k].is_ascii_whitespace() && !matches!(b[k], b'=' | b'>' | b'/') {
                k += 1;
            }
            let an = lower[n0..k].to_string();
            while k < b.len() && b[k].is_ascii_whitespace() {
                k += 1;
            }
            let mut value = None;
            if k < b.len() && b[k] == b'=' {
                k += 1;
                while k < b.len() && b[k].is_ascii_whitespace() {
                    k += 1;
                }
                if k < b.len() && (b[k] == b'"' || b[k] == b'\'') {
                    let q = b[k];
                    let v0 = k + 1;
                    k = v0;
                    while k < b.len() && b[k] != q {
                        k += 1;
                    }
                    value = Some(src[v0..k.min(b.len())].to_string());
                    k = (k + 1).min(b.len());
                } else {
                    let v0 = k;
                    while k < b.len() && !b[k].is_ascii_whitespace() && b[k] != b'>' {
                        k += 1;
                    }
                    value = Some(src[v0..k].to_string());
                }
            }
            if !an.is_empty() {
                attrs.push((an, value));
            } else {
                k += 1;
            }
        };
        let text_end = if RAW_TEXT.contains(&name.as_str()) {
            let close = format!("</{name}");
            Some(lower[end..].find(&close).map(|e| end + e).unwrap_or(src.len()))
        } else {
            None
        };
        i = text_end.unwrap_or(end);
        tags.push(Tag { name, start: at, end, attrs, text_end });
    }
    tags
}

fn esc_attr(v: &str) -> String {
    v.replace('&', "&amp;").replace('"', "&quot;")
}

fn esc_text(v: &str) -> String {
    v.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn render(el: &Element) -> String {
    let mut s = format!("<{}", el.name);
    for (k, v) in &el.attrs {
        s.push_str(&format!(" {k}=\"{}\"", esc_attr(v)));
    }
    s.push('>');
    if let Some(t) = &el.text {
        // Script and style text is raw; anything else is escaped.
        s.push_str(&if RAW_TEXT[..2].contains(&el.name.as_str()) { t.clone() } else { esc_text(t) });
    }
    if !matches!(el.name.as_str(), "meta" | "link" | "br" | "img" | "input" | "hr" | "base") {
        s.push_str(&format!("</{}>", el.name));
    }
    s
}

/// One compound selector: a tag name and attribute tests.
struct Sel {
    name: Option<String>,
    /// `:not(:empty)`: the element has text (tauri-codegen hashes inline scripts).
    not_empty: bool,
    tests: Vec<(String, Option<(bool, String)>)>,
}

fn parse_selector(s: &str) -> Option<Vec<Sel>> {
    s.split(',')
        .map(|part| {
            let p = part.trim();
            let (p, not_empty) = match p.strip_suffix(":not(:empty)") {
                Some(q) => (q, true),
                None => (p, false),
            };
            let (name, mut rest) = match p.find('[') {
                Some(i) => (&p[..i], &p[i..]),
                None => (p, ""),
            };
            if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return None;
            }
            let mut tests = Vec::new();
            while let Some(r) = rest.strip_prefix('[') {
                let close = r.find(']')?;
                let t = &r[..close];
                rest = &r[close + 1..];
                let (key, val) = if let Some((k, v)) = t.split_once("^=") {
                    (k, Some((true, v)))
                } else if let Some((k, v)) = t.split_once('=') {
                    (k, Some((false, v)))
                } else {
                    (t, None)
                };
                let unq = |v: &str| v.trim().trim_matches(|c| c == '\'' || c == '"').to_string();
                tests.push((key.trim().to_ascii_lowercase(), val.map(|(pre, v)| (pre, unq(v)))));
            }
            if !rest.trim().is_empty() {
                return None;
            }
            Some(Sel { name: (!name.is_empty()).then(|| name.to_ascii_lowercase()), not_empty, tests })
        })
        .collect()
}

impl Inner {
    fn attrs_of(&self, i: usize) -> Vec<(String, Option<String>)> {
        match self.edits.get(&i) {
            Some(e) => e.attrs.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect(),
            None => self.tags[i].attrs.clone(),
        }
    }

    /// A raw-text element's text as it now stands (edited or the page's).
    fn text_of(&self, i: usize) -> String {
        let t = &self.tags[i];
        match (self.edits.get(&i).and_then(|e| e.text.clone()), t.text_end) {
            (Some(x), _) => x,
            (None, Some(te)) => self.src[t.end..te].to_string(),
            _ => String::new(),
        }
    }

    fn edit(&mut self, i: usize) -> &mut Element {
        let t = &self.tags[i];
        let attrs = t.attrs.iter().map(|(k, v)| (k.clone(), v.clone().unwrap_or_default())).collect();
        let name = t.name.clone();
        self.edits.entry(i).or_insert_with(|| Element { name, attrs, text: None })
    }

    fn render_ids(&self, ids: &[usize]) -> String {
        ids.iter().map(|&id| render(&self.made[id])).collect()
    }

    /// The page with every edit applied: (from, to, text) pieces over the source, insertions being from == to.
    fn html(&self) -> String {
        let src = &self.src;
        let mut parts: Vec<(usize, usize, String)> = Vec::new();
        let head = self.tags.iter().position(|t| t.name == "head");
        match head {
            Some(h) => {
                let at = self.tags[h].end;
                parts.push((at, at, self.render_ids(&self.head_first)));
                if !self.head_last.is_empty() {
                    // Before </head>, outside any raw text; else just after <head>.
                    let lower = src.to_ascii_lowercase();
                    let mut close = None;
                    let mut from = at;
                    while let Some(e) = lower[from..].find("</head") {
                        let c = from + e;
                        if !self.tags.iter().any(|t| t.text_end.is_some_and(|te| t.end <= c && c < te)) {
                            close = Some(c);
                            break;
                        }
                        from = c + 6;
                    }
                    let c = close.unwrap_or(at);
                    parts.push((c, c, self.render_ids(&self.head_last)));
                }
            }
            None if self.made_head => {
                let at = self.tags.iter().find(|t| t.name == "html").map(|t| t.end).unwrap_or(0);
                parts.push((at, at, format!("<head>{}{}</head>", self.render_ids(&self.head_first), self.render_ids(&self.head_last))));
            }
            None => {}
        }
        for (i, e) in &self.edits {
            let t = &self.tags[*i];
            let mut tag = format!("<{}", t.name);
            for (k, v) in &e.attrs {
                tag.push_str(&format!(" {k}=\"{}\"", esc_attr(v)));
            }
            tag.push('>');
            parts.push((t.start, t.end, tag));
            if let (Some(te), Some(text)) = (t.text_end, &e.text) {
                parts.push((t.end, te, text.clone()));
            }
        }
        // By place; at one place insertions first, in the order made (stable sort).
        parts.sort_by_key(|(a, b, _)| (*a, b != a));
        let mut out = String::with_capacity(src.len() + 256);
        let mut pos = 0;
        for (a, b, text) in parts {
            out.push_str(&src[pos..a.max(pos)]);
            out.push_str(&text);
            pos = pos.max(b);
        }
        out.push_str(&src[pos..]);
        out
    }
}

impl From<String> for Document {
    fn from(src: String) -> Self {
        let tags = scan(&src);
        let inner = Rc::new(RefCell::new(Inner { src, tags, ..Default::default() }));
        Document { tree: Tree { inner: inner.clone() }, inner }
    }
}

impl From<&str> for Document {
    fn from(src: &str) -> Self {
        Document::from(src.to_string())
    }
}

impl Document {
    fn node(&self, kind: Kind) -> NodeRef<'_> {
        NodeRef { kind, inner: self.inner.clone(), _doc: PhantomData }
    }

    pub fn head(&self) -> Option<NodeRef<'_>> {
        let i = self.inner.borrow();
        let found = i.tags.iter().position(|t| t.name == "head").map(Kind::Page);
        let made = i.made_head.then_some(Kind::NewHead);
        drop(i);
        found.or(made).map(|k| self.node(k))
    }

    pub fn html_root(&self) -> NodeRef<'_> {
        let k = self.inner.borrow().tags.iter().position(|t| t.name == "html").map(Kind::Page).unwrap_or(Kind::NoHtml);
        self.node(k)
    }

    pub fn select(&self, selector: &str) -> Selection<'_> {
        let Some(sels) = parse_selector(selector) else { return Selection { nodes: vec![] } };
        let i = self.inner.borrow();
        let hits: Vec<usize> = (0..i.tags.len())
            .filter(|&n| {
                let attrs = i.attrs_of(n);
                sels.iter().any(|s| {
                    s.name.as_deref().is_none_or(|nm| nm == i.tags[n].name)
                        && (!s.not_empty || !i.text_of(n).is_empty())
                        && s.tests.iter().all(|(k, test)| match attrs.iter().find(|(a, _)| a == k) {
                            None => false,
                            Some((_, v)) => match test {
                                None => true,
                                Some((prefix, want)) => {
                                    let v = v.as_deref().unwrap_or("");
                                    if *prefix { v.starts_with(want.as_str()) } else { v == want }
                                }
                            },
                        })
                })
            })
            .collect();
        drop(i);
        Selection { nodes: hits.into_iter().map(|n| self.node(Kind::Page(n))).collect() }
    }

    pub fn html(&self) -> String {
        self.inner.borrow().html()
    }
}

impl Tree {
    pub fn new_element(&self, name: &str) -> NodeRef<'_> {
        let name = name.to_ascii_lowercase();
        if name == "head" {
            return NodeRef { kind: Kind::NewHead, inner: self.inner.clone(), _doc: PhantomData };
        }
        let mut i = self.inner.borrow_mut();
        i.made.push(Element { name, ..Default::default() });
        let id = i.made.len() - 1;
        drop(i);
        NodeRef { kind: Kind::Made(id), inner: self.inner.clone(), _doc: PhantomData }
    }
}

impl<'a> NodeRef<'a> {
    fn is_head(&self) -> bool {
        match self.kind {
            Kind::NewHead => true,
            Kind::Page(n) => self.inner.borrow().tags[n].name == "head",
            _ => false,
        }
    }

    fn is_html(&self) -> bool {
        match self.kind {
            Kind::NoHtml => true,
            Kind::Page(n) => self.inner.borrow().tags[n].name == "html",
            _ => false,
        }
    }

    /// Children of the head (new elements only); a new head into the html root.
    pub fn prepend_child(&self, child: &NodeRef) {
        if child.kind == Kind::NewHead && self.is_html() {
            self.inner.borrow_mut().made_head = true;
        } else if let (true, Kind::Made(id)) = (self.is_head(), child.kind) {
            self.inner.borrow_mut().head_first.insert(0, id);
        }
    }

    pub fn append_child(&self, child: &NodeRef) {
        if child.kind == Kind::NewHead && self.is_html() {
            self.inner.borrow_mut().made_head = true;
        } else if let (true, Kind::Made(id)) = (self.is_head(), child.kind) {
            self.inner.borrow_mut().head_last.push(id);
        }
    }

    pub fn attr(&self, name: &str) -> Option<String> {
        let i = self.inner.borrow();
        let name = name.to_ascii_lowercase();
        match self.kind {
            Kind::Page(n) => i.attrs_of(n).into_iter().find(|(k, _)| *k == name).map(|(_, v)| v.unwrap_or_default()),
            Kind::Made(id) => i.made[id].attrs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone()),
            _ => None,
        }
    }

    /// The text of a script/style/textarea/title (of the page or made here); empty for anything else.
    pub fn text(&self) -> String {
        let i = self.inner.borrow();
        match self.kind {
            Kind::Page(n) => i.text_of(n),
            Kind::Made(id) => i.made[id].text.clone().unwrap_or_default(),
            _ => String::new(),
        }
    }

    pub fn has_attr(&self, name: &str) -> bool {
        self.attr(name).is_some()
    }

    pub fn set_attr(&self, name: &str, value: &str) {
        let mut i = self.inner.borrow_mut();
        let name = name.to_ascii_lowercase();
        let attrs = match self.kind {
            Kind::Page(n) => &mut i.edit(n).attrs,
            Kind::Made(id) => &mut i.made[id].attrs,
            _ => return,
        };
        match attrs.iter_mut().find(|(k, _)| *k == name) {
            Some(a) => a.1 = value.to_string(),
            None => attrs.push((name, value.to_string())),
        }
    }

    pub fn remove_attr(&self, name: &str) {
        let mut i = self.inner.borrow_mut();
        let name = name.to_ascii_lowercase();
        match self.kind {
            Kind::Page(n) => i.edit(n).attrs.retain(|(k, _)| *k != name),
            Kind::Made(id) => i.made[id].attrs.retain(|(k, _)| *k != name),
            _ => {}
        }
    }

    /// The text of a new element, or of the page's script/style/textarea/title.
    pub fn set_text<T: AsRef<str>>(&self, text: T) {
        let mut i = self.inner.borrow_mut();
        match self.kind {
            Kind::Page(n) if i.tags[n].text_end.is_some() => i.edit(n).text = Some(text.as_ref().to_string()),
            Kind::Made(id) => i.made[id].text = Some(text.as_ref().to_string()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// wry's inject_scripts_into_html, call for call.
    fn wry_inject(page: &str, scripts: &[&str]) -> String {
        let document = Document::from(page);
        let head = document.head().unwrap_or_else(|| {
            let html = document.html_root();
            let head = document.tree.new_element("head");
            html.prepend_child(&head);
            head
        });
        for s in scripts.iter().rev() {
            let tag = document.tree.new_element("script");
            tag.set_text(*s);
            head.prepend_child(&tag);
        }
        document.html()
    }

    #[test]
    fn wry_scripts_go_first_in_head() {
        assert_eq!(wry_inject("<!doctype html><html lang=en><HEAD data-x><title>t</title></HEAD><body></body></html>", &["a()", "b()"]),
            "<!doctype html><html lang=en><HEAD data-x><script>a()</script><script>b()</script><title>t</title></HEAD><body></body></html>");
        assert_eq!(wry_inject("<html><header>x</header></html>", &["a()"]), "<html><head><script>a()</script></head><header>x</header></html>");
        assert_eq!(wry_inject("<p>bare</p>", &["a()"]), "<head><script>a()</script></head><p>bare</p>");
        assert_eq!(wry_inject("<html><head></head></html>", &[]), "<html><head></head></html>");
    }

    #[test]
    fn untouched_pages_round_trip() {
        let page = "<!doctype html>\n<!-- <head> in a comment --><html><head><meta charset=utf-8><script>if (a<b) x='</div>'</script></head><body class='x'>é</body></html>";
        assert_eq!(Document::from(page).html(), page);
    }

    #[test]
    fn tauri_html2_calls() {
        // inject_nonce: style and script[src^='http'], skipping a nonce that is there.
        let d = Document::from("<html><head><style>a{}</style><script src=\"http://x/a.js\"></script><script src='/b.js'></script><style nonce=\"n0\"></style></head></html>".to_string());
        for el in d.select("style").nodes() {
            if el.attr("nonce").is_some() {
                continue;
            }
            el.set_attr("nonce", "S");
        }
        for el in d.select("script[src^='http']").nodes() {
            el.set_attr("nonce", "J");
        }
        assert_eq!(d.select("script[src]").length(), 2);
        assert_eq!(d.html(), "<html><head><style nonce=\"S\">a{}</style><script src=\"http://x/a.js\" nonce=\"J\"></script><script src='/b.js'></script><style nonce=\"n0\"></style></head></html>");
        // inject_csp: a meta at the end of head; append_script_to_head: a script first.
        let d = Document::from("<html><head><title>x</title></head><body></body></html>");
        let head = d.head().unwrap();
        let meta = d.tree.new_element("meta");
        meta.set_attr("http-equiv", "Content-Security-Policy");
        meta.set_attr("content", "default-src 'self' \"x\"");
        head.append_child(&meta);
        let s = d.tree.new_element("script");
        s.set_text("go()");
        head.prepend_child(&s);
        assert_eq!(d.html(), "<html><head><script>go()</script><title>x</title><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'self' &quot;x&quot;\"></head><body></body></html>");
        // inline_isolation: the file's text replaces src.
        let d = Document::from("<html><head><script src=\"/iso.js\"></script></head></html>");
        for s in d.select("script[src]").nodes() {
            assert_eq!(s.attr("src").as_deref(), Some("/iso.js"));
            s.set_text("inlined()");
            s.remove_attr("src");
        }
        assert_eq!(d.html(), "<html><head><script>inlined()</script></head></html>");
        assert_eq!(d.select("div > p").length(), 0, "unsupported selectors select nothing");
        // tauri-codegen: hashes of inline scripts.
        let d = Document::from("<head><script>a()</script><script src=x></script><script></script></head>");
        let texts: Vec<String> = d.select("script:not(:empty)").iter().map(|e| e.text()).collect();
        assert_eq!(texts, vec!["a()".to_string()]);
    }
}
