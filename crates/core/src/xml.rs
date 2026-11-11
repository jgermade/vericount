//! Minimal XML helpers: a writer with escaping and a namespace-unaware
//! element tree for reading SOAP messages.

use std::fmt;

use quick_xml::{events::Event, Reader};

/// Escapes text for element content and attribute values.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Builds XML text. Element names are written as given, with their prefix.
#[derive(Debug, Default)]
pub struct Writer {
    out: String,
    open: Vec<&'static str>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens an element; `attributes` is written verbatim after the name.
    pub fn open_with(&mut self, name: &'static str, attributes: &str) -> &mut Self {
        self.out.push('<');
        self.out.push_str(name);
        self.out.push_str(attributes);
        self.out.push('>');
        self.open.push(name);
        self
    }

    pub fn open(&mut self, name: &'static str) -> &mut Self {
        self.open_with(name, "")
    }

    pub fn close(&mut self) -> &mut Self {
        let name = self.open.pop().expect("close without open");
        self.out.push_str("</");
        self.out.push_str(name);
        self.out.push('>');
        self
    }

    /// Writes `<name>text</name>` with the text escaped.
    pub fn leaf(&mut self, name: &'static str, text: &str) -> &mut Self {
        self.open(name);
        self.out.push_str(&escape(text));
        self.close()
    }

    /// Inserts already serialized XML.
    pub fn raw(&mut self, xml: &str) -> &mut Self {
        self.out.push_str(xml);
        self
    }

    pub fn finish(self) -> String {
        assert!(self.open.is_empty(), "unclosed elements: {:?}", self.open);
        self.out
    }
}

/// An element with its namespace prefix removed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Element {
    pub name: String,
    pub text: String,
    pub children: Vec<Element>,
}

impl Element {
    /// First direct child with this local name.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.name == name)
    }

    /// Direct children with this local name.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |child| child.name == name)
    }

    /// Follows a path of direct children.
    pub fn at(&self, path: &[&str]) -> Option<&Element> {
        path.iter()
            .try_fold(self, |element, name| element.child(name))
    }

    /// Trimmed text of the element at `path`, if it exists.
    pub fn text_at(&self, path: &[&str]) -> Option<&str> {
        self.at(path).map(|element| element.text.trim())
    }

    /// First descendant (depth first) with this local name.
    pub fn find(&self, name: &str) -> Option<&Element> {
        self.children.iter().find_map(|child| {
            if child.name == name {
                Some(child)
            } else {
                child.find(name)
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid XML: {}", self.0)
    }
}

impl std::error::Error for ParseError {}

fn local_name(name: &[u8]) -> String {
    let name = String::from_utf8_lossy(name);
    name.rsplit(':').next().unwrap_or_default().to_owned()
}

/// Parses a document into its root element. DTDs are rejected, so there
/// are no custom entities to expand.
pub fn parse(xml: &str) -> Result<Element, ParseError> {
    let error = |e: &dyn fmt::Display| ParseError(e.to_string());
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<Element> = Vec::new();
    let mut root = None;

    let mut finish = |stack: &mut Vec<Element>, element: Element| match stack.last_mut() {
        Some(parent) => {
            parent.children.push(element);
            Ok(())
        }
        None if root.is_none() => {
            root = Some(element);
            Ok(())
        }
        None => Err(ParseError("more than one root element".to_owned())),
    };

    loop {
        match reader.read_event().map_err(|e| error(&e))? {
            Event::Start(start) => stack.push(Element {
                name: local_name(start.name().as_ref()),
                ..Element::default()
            }),
            Event::Empty(start) => {
                let element = Element {
                    name: local_name(start.name().as_ref()),
                    ..Element::default()
                };
                finish(&mut stack, element)?;
            }
            Event::End(_) => {
                let element = stack
                    .pop()
                    .ok_or_else(|| ParseError("unexpected end tag".to_owned()))?;
                finish(&mut stack, element)?;
            }
            Event::Text(text) => {
                if let Some(element) = stack.last_mut() {
                    element
                        .text
                        .push_str(&text.decode().map_err(|e| error(&e))?);
                }
            }
            Event::GeneralRef(reference) => {
                let name = reference.decode().map_err(|e| error(&e))?;
                let resolved = match &*name {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    _ => reference
                        .resolve_char_ref()
                        .map_err(|e| error(&e))?
                        .ok_or_else(|| ParseError(format!("unknown entity &{name};")))?,
                };
                if let Some(element) = stack.last_mut() {
                    element.text.push(resolved);
                }
            }
            Event::CData(data) => {
                if let Some(element) = stack.last_mut() {
                    element.text.push_str(&String::from_utf8_lossy(&data));
                }
            }
            Event::DocType(_) => return Err(ParseError("DTDs are not allowed".to_owned())),
            Event::Eof => break,
            Event::Decl(_) | Event::Comment(_) | Event::PI(_) => {}
        }
    }
    if !stack.is_empty() {
        return Err(ParseError("unclosed element".to_owned()));
    }
    root.ok_or_else(|| ParseError("no root element".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_back() {
        let mut writer = Writer::new();
        writer
            .open_with("a:root", " xmlns:a=\"urn:a\"")
            .leaf("a:name", "Tom & <Jerry> \"SL\"")
            .open("a:list")
            .leaf("a:item", "1")
            .leaf("a:item", "2")
            .close()
            .close();
        let xml = writer.finish();
        assert!(xml.contains("Tom &amp; &lt;Jerry&gt; &quot;SL&quot;"));

        let root = parse(&xml).unwrap();
        assert_eq!(root.name, "root");
        assert_eq!(root.text_at(&["name"]), Some("Tom & <Jerry> \"SL\""));
        let items: Vec<_> = root
            .at(&["list"])
            .unwrap()
            .children_named("item")
            .map(|item| item.text.as_str())
            .collect();
        assert_eq!(items, ["1", "2"]);
        assert_eq!(root.find("item").unwrap().text, "1");
    }

    #[test]
    fn rejects_malformed_documents_and_dtds() {
        for bad in [
            "",
            "<a>",
            "<a></b>",
            "<a/><b/>",
            "<a>&unknown;</a>",
            "<!DOCTYPE a [<!ENTITY x \"y\">]><a>&x;</a>",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert_eq!(parse("<a>&#65;&#x42;</a>").unwrap().text, "AB");
    }
}
