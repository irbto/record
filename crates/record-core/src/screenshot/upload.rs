//! Upload destinations described by ShareX custom uploader (`.sxcu`) files.
//!
//! A `.sxcu` file is a small JSON document: where to send the request, which
//! headers and form fields to add, and how to find the public link in the
//! response. This module reads that format, builds the request body, and
//! reads the link from the response. It performs no network access. The
//! Windows `http` module sends the request.

use std::{
    env, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use super::format::ImageFormat;

/// Deepest nesting that the JSON reader accepts.
const JSON_DEPTH_LIMIT: usize = 64;

/// One parsed JSON value. Numbers keep their source text.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(String),
    Text(String),
    List(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Parses one complete JSON document.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut reader = JsonReader {
            bytes: text.as_bytes(),
            position: 0,
        };
        let value = reader.value(0)?;
        reader.skip_space();
        if reader.position != reader.bytes.len() {
            return Err(reader.fail("unexpected text after the JSON value"));
        }
        Ok(value)
    }

    /// Returns the member of an object. Names compare without case, because
    /// `.sxcu` files from different tools do not agree on it.
    fn member(&self, name: &str) -> Option<&Self> {
        match self {
            Self::Object(members) => members
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// Returns text for string, number, and boolean values.
    fn as_text(&self) -> Option<String> {
        match self {
            Self::Text(text) | Self::Number(text) => Some(text.clone()),
            Self::Bool(flag) => Some(flag.to_string()),
            _ => None,
        }
    }

    /// Follows a path such as `data.link` or `files[0].url`.
    pub(crate) fn lookup(&self, path: &str) -> Option<&Self> {
        let mut current = self;
        for part in path.split('.').filter(|part| !part.is_empty()) {
            let (name, indexes) = part.split_once('[').map_or((part, ""), |(name, rest)| {
                (name, &part[part.len() - rest.len() - 1..])
            });
            if !name.is_empty() {
                current = match current {
                    Self::Object(members) => members
                        .iter()
                        .find(|(key, _)| key == name)
                        .map(|(_, value)| value)?,
                    _ => return None,
                };
            }
            for index in indexes.split('[').filter(|index| !index.is_empty()) {
                let index: usize = index.strip_suffix(']')?.parse().ok()?;
                current = match current {
                    Self::List(items) => items.get(index)?,
                    _ => return None,
                };
            }
        }
        Some(current)
    }
}

/// A cursor over JSON source bytes.
struct JsonReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl JsonReader<'_> {
    /// Formats a parse error with the byte position.
    fn fail(&self, message: &str) -> String {
        format!("invalid JSON at byte {}: {message}", self.position)
    }

    /// Skips spaces, tabs, and line ends.
    fn skip_space(&mut self) {
        while matches!(
            self.bytes.get(self.position),
            Some(b' ' | b'\t' | b'\r' | b'\n')
        ) {
            self.position += 1;
        }
    }

    /// Consumes a fixed word such as `true`.
    fn word(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.bytes[self.position..].starts_with(word.as_bytes()) {
            self.position += word.len();
            Ok(value)
        } else {
            Err(self.fail("unknown value"))
        }
    }

    /// Reads any value.
    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > JSON_DEPTH_LIMIT {
            return Err(self.fail("the document is nested too deeply"));
        }
        self.skip_space();
        match self.bytes.get(self.position) {
            None => Err(self.fail("the document ends too early")),
            Some(b'n') => self.word("null", Json::Null),
            Some(b't') => self.word("true", Json::Bool(true)),
            Some(b'f') => self.word("false", Json::Bool(false)),
            Some(b'"') => self.text().map(Json::Text),
            Some(b'[') => {
                self.position += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    if self.bytes.get(self.position) == Some(&b']') {
                        self.position += 1;
                        return Ok(Json::List(items));
                    }
                    if !items.is_empty() {
                        self.expect(b',')?;
                    }
                    items.push(self.value(depth + 1)?);
                }
            }
            Some(b'{') => {
                self.position += 1;
                let mut members = Vec::new();
                loop {
                    self.skip_space();
                    if self.bytes.get(self.position) == Some(&b'}') {
                        self.position += 1;
                        return Ok(Json::Object(members));
                    }
                    if !members.is_empty() {
                        self.expect(b',')?;
                        self.skip_space();
                    }
                    let name = self.text()?;
                    self.skip_space();
                    self.expect(b':')?;
                    members.push((name, self.value(depth + 1)?));
                }
            }
            Some(_) => {
                let start = self.position;
                while matches!(
                    self.bytes.get(self.position),
                    Some(b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                ) {
                    self.position += 1;
                }
                let number = &self.bytes[start..self.position];
                let text = String::from_utf8_lossy(number).into_owned();
                if text.parse::<f64>().is_err() {
                    return Err(self.fail("unknown value"));
                }
                Ok(Json::Number(text))
            }
        }
    }

    /// Consumes one required punctuation byte.
    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.bytes.get(self.position) == Some(&byte) {
            self.position += 1;
            Ok(())
        } else {
            Err(self.fail(&format!("expected `{}`", byte as char)))
        }
    }

    /// Reads four hexadecimal digits of a `\u` escape.
    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .bytes
            .get(self.position..self.position + 4)
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u32::from_str_radix(digits, 16).ok())
            .ok_or_else(|| self.fail("invalid \\u escape"))?;
        self.position += 4;
        Ok(digits)
    }

    /// Reads one quoted string and resolves its escapes.
    fn text(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut output = Vec::new();
        loop {
            let byte = *self
                .bytes
                .get(self.position)
                .ok_or_else(|| self.fail("the string does not end"))?;
            self.position += 1;
            match byte {
                b'"' => break,
                b'\\' => {
                    let escape = *self
                        .bytes
                        .get(self.position)
                        .ok_or_else(|| self.fail("the string does not end"))?;
                    self.position += 1;
                    let character = match escape {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let mut code = self.hex4()?;
                            if (0xd800..0xdc00).contains(&code)
                                && self.bytes[self.position..].starts_with(b"\\u")
                            {
                                self.position += 2;
                                let low = self.hex4()?;
                                code = 0x1_0000 + ((code - 0xd800) << 10) + (low & 0x3ff);
                            }
                            char::from_u32(code).unwrap_or('\u{fffd}')
                        }
                        other => other as char,
                    };
                    output.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
                }
                other => output.push(other),
            }
        }
        String::from_utf8(output).map_err(|_| self.fail("the string is not valid UTF-8"))
    }
}

/// How an uploader places the image in the request body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BodyKind {
    /// A `multipart/form-data` body with the arguments and one file part.
    Multipart,
    /// The raw image bytes.
    Binary,
}

/// One upload destination.
#[derive(Clone, Debug, PartialEq)]
pub struct Uploader {
    /// Name shown in the hub and in messages.
    pub name: String,
    method: String,
    request_url: String,
    parameters: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: BodyKind,
    arguments: Vec<(String, String)>,
    file_form_name: String,
    url: String,
    deletion_url: String,
    error_message: String,
}

/// One HTTP request, ready to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HttpRequest {
    pub(crate) method: String,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

/// One received HTTP response.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HttpResponse {
    pub(crate) status: u32,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

/// The links that a destination returned for one upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadResult {
    /// Public link of the uploaded image.
    pub url: String,
    /// Link that deletes the upload, when the destination gives one.
    pub deletion_url: Option<String>,
}

/// The parts of an `http` or `https` address that a connection needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UrlParts {
    pub(crate) secure: bool,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) path: String,
}

impl UrlParts {
    /// Splits an address into scheme, host, port, and path with query.
    pub(crate) fn parse(url: &str) -> Result<Self, String> {
        let (secure, rest) = if let Some(rest) = strip_prefix_ignore_case(url, "https://") {
            (true, rest)
        } else if let Some(rest) = strip_prefix_ignore_case(url, "http://") {
            (false, rest)
        } else {
            return Err(format!(
                "the address {url} must start with http:// or https://"
            ));
        };
        let split = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, path) = rest.split_at(split);
        if authority.is_empty() || authority.contains(['@', '[', ' ']) {
            return Err(format!("the address {url} has an unsupported host"));
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (
                host,
                port.parse::<u16>()
                    .map_err(|_| format!("the address {url} has an invalid port"))?,
            ),
            None => (authority, if secure { 443 } else { 80 }),
        };
        let path = match path.chars().next() {
            None => "/".to_owned(),
            Some('?') => format!("/{path}"),
            Some(_) => path.to_owned(),
        };
        Ok(Self {
            secure,
            host: host.to_owned(),
            port,
            path,
        })
    }
}

/// Removes an ASCII prefix without regard to case.
fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// Percent-encodes a query string component.
fn encode_component(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Reads an object of string values as ordered pairs.
fn pairs(document: &Json, name: &str) -> Vec<(String, String)> {
    match document.member(name) {
        Some(Json::Object(members)) => members
            .iter()
            .filter_map(|(key, value)| Some((key.clone(), value.as_text()?)))
            .collect(),
        _ => Vec::new(),
    }
}

/// Describes one built-in upload destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Preset {
    /// Identifier for the command line and the settings files.
    pub id: &'static str,
    /// Name shown in the hub.
    pub name: &'static str,
    /// What the destination calls its key. `None` when it needs no key.
    pub key_label: Option<&'static str>,
    /// Where the user gets the key.
    pub key_source: &'static str,
    /// One short fact for the destination list.
    pub note: &'static str,
}

/// Every built-in destination in list order.
pub const PRESETS: [Preset; 4] = [
    Preset {
        id: "imgur",
        name: "Imgur",
        key_label: Some("client ID"),
        key_source: "api.imgur.com/oauth2/addclient",
        note: "needs a client ID",
    },
    Preset {
        id: "imgbb",
        name: "ImgBB",
        key_label: Some("API key"),
        key_source: "api.imgbb.com",
        note: "needs an API key",
    },
    Preset {
        id: "catbox",
        name: "Catbox",
        key_label: None,
        key_source: "",
        note: "no account",
    },
    Preset {
        id: "litterbox",
        name: "Litterbox",
        key_label: None,
        key_source: "",
        note: "no account, deleted after 72 hours",
    },
];

impl Preset {
    /// Finds a built-in destination by its identifier, without regard to case.
    #[must_use]
    pub fn find(id: &str) -> Option<&'static Self> {
        PRESETS
            .iter()
            .find(|preset| preset.id.eq_ignore_ascii_case(id))
    }

    /// Returns the stored key of this destination.
    #[must_use]
    pub fn key(&self) -> Option<String> {
        let variable = format!("RECORD_{}_KEY", self.id.to_ascii_uppercase());
        let mut sources = vec![
            env::var(variable).ok(),
            settings_directory()
                .and_then(|directory| fs::read_to_string(directory.join(self.key_file())).ok()),
        ];
        if self.id == "imgur" {
            // Earlier versions stored the Imgur client ID under these names.
            sources.push(env::var("RECORD_IMGUR_CLIENT_ID").ok());
            sources.push(settings_directory().and_then(|directory| {
                fs::read_to_string(directory.join("imgur-client-id.txt")).ok()
            }));
        }
        sources
            .into_iter()
            .flatten()
            .map(|key| key.trim().to_owned())
            .find(|key| !key.is_empty())
    }

    /// Reports whether the destination can upload now.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.key_label.is_none() || self.key().is_some()
    }

    /// Stores the key of this destination for later runs.
    pub fn save_key(&self, key: &str) -> Result<(), String> {
        let key = key.trim();
        if !valid_key(key) {
            return Err(format!(
                "the {} {} has 6 to 128 letters, digits, and dashes",
                self.name,
                self.key_label.unwrap_or("key")
            ));
        }
        write_setting(&self.key_file(), key)
    }

    /// Returns the name of the settings file that holds the key.
    fn key_file(&self) -> String {
        format!("{}-key.txt", self.id)
    }
}

impl Uploader {
    /// Reads a ShareX custom uploader file.
    ///
    /// `fallback_name` names the uploader when the file has no `Name`.
    pub fn from_sxcu(text: &str, fallback_name: &str) -> Result<Self, String> {
        let document = Json::parse(text.trim_start_matches('\u{feff}'))?;
        let field = |name: &str| {
            document
                .member(name)
                .and_then(Json::as_text)
                .unwrap_or_default()
        };
        let request_url = field("RequestURL");
        if request_url.is_empty() {
            return Err("the uploader file has no RequestURL".to_owned());
        }
        UrlParts::parse(&request_url)?;
        let body = match field("Body").to_ascii_lowercase().as_str() {
            "" | "multipartformdata" => BodyKind::Multipart,
            "binary" => BodyKind::Binary,
            other => {
                return Err(format!(
                    "the body type {other} cannot carry an image file; use MultipartFormData or Binary"
                ));
            }
        };
        let name = field("Name");
        let method = field("RequestMethod");
        let file_form_name = field("FileFormName");
        Ok(Self {
            name: if name.is_empty() {
                fallback_name.to_owned()
            } else {
                name
            },
            method: if method.is_empty() {
                "POST".to_owned()
            } else {
                method.to_ascii_uppercase()
            },
            request_url,
            parameters: pairs(&document, "Parameters"),
            headers: pairs(&document, "Headers"),
            body,
            arguments: pairs(&document, "Arguments"),
            file_form_name: if file_form_name.is_empty() {
                "file".to_owned()
            } else {
                file_form_name
            },
            url: field("URL"),
            deletion_url: field("DeletionURL"),
            error_message: field("ErrorMessage"),
        })
    }

    /// Returns a built-in destination by its identifier.
    ///
    /// `key` is the API key of a destination that needs one.
    #[must_use]
    pub fn preset(id: &str, key: &str) -> Option<Self> {
        let preset = Preset::find(id)?;
        let key = key.trim();
        let pair = |name: &str, value: &str| (name.to_owned(), value.to_owned());
        let mut uploader = Self {
            name: preset.name.to_owned(),
            method: "POST".to_owned(),
            request_url: String::new(),
            parameters: Vec::new(),
            headers: Vec::new(),
            body: BodyKind::Multipart,
            arguments: Vec::new(),
            file_form_name: "file".to_owned(),
            url: String::new(),
            deletion_url: String::new(),
            error_message: String::new(),
        };
        match preset.id {
            "imgur" => {
                uploader.request_url = "https://api.imgur.com/3/image".to_owned();
                uploader.headers = vec![pair("Authorization", &format!("Client-ID {key}"))];
                uploader.file_form_name = "image".to_owned();
                uploader.url = "{json:data.link}".to_owned();
                uploader.deletion_url =
                    "https://imgur.com/delete/{json:data.deletehash}".to_owned();
                uploader.error_message = "{json:data.error}".to_owned();
            }
            "imgbb" => {
                uploader.request_url = "https://api.imgbb.com/1/upload".to_owned();
                uploader.parameters = vec![pair("key", key)];
                uploader.file_form_name = "image".to_owned();
                uploader.url = "{json:data.url}".to_owned();
                uploader.deletion_url = "{json:data.delete_url}".to_owned();
                uploader.error_message = "{json:error.message}".to_owned();
            }
            "catbox" => {
                uploader.request_url = "https://catbox.moe/user/api.php".to_owned();
                uploader.arguments = vec![pair("reqtype", "fileupload")];
                uploader.file_form_name = "fileToUpload".to_owned();
                uploader.error_message = "{response}".to_owned();
            }
            "litterbox" => {
                uploader.request_url =
                    "https://litterbox.catbox.moe/resources/internals/api.php".to_owned();
                uploader.arguments = vec![pair("reqtype", "fileupload"), pair("time", "72h")];
                uploader.file_form_name = "fileToUpload".to_owned();
                uploader.error_message = "{response}".to_owned();
            }
            _ => return None,
        }
        Some(uploader)
    }

    /// Returns the built-in Imgur destination for an Imgur API client ID.
    #[must_use]
    pub fn imgur(client_id: &str) -> Self {
        Self::preset("imgur", client_id).unwrap_or_else(|| unreachable!("imgur is a preset"))
    }

    /// Builds the HTTP request that uploads one image file.
    ///
    /// The extension of the file name selects the media type.
    pub(crate) fn request(&self, file_name: &str, png: &[u8]) -> HttpRequest {
        let fill = |text: &str| text.replace("{filename}", file_name);
        let mime = ImageFormat::from_path(Path::new(file_name))
            .unwrap_or_default()
            .mime();
        let mut url = self.request_url.clone();
        for (index, (name, value)) in self.parameters.iter().enumerate() {
            let joiner = if index == 0 && !url.contains('?') {
                '?'
            } else {
                '&'
            };
            url.push(joiner);
            url.push_str(&encode_component(name));
            url.push('=');
            url.push_str(&encode_component(&fill(value)));
        }
        let mut headers: Vec<(String, String)> = self
            .headers
            .iter()
            .filter(|(name, _)| !name.eq_ignore_ascii_case("content-type"))
            .map(|(name, value)| (name.clone(), fill(value)))
            .collect();
        let body = match self.body {
            BodyKind::Binary => {
                headers.push(("Content-Type".to_owned(), mime.to_owned()));
                png.to_vec()
            }
            BodyKind::Multipart => {
                let stamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_nanos());
                let boundary = format!("----record{stamp:032x}");
                let quote = |text: &str| text.replace(['"', '\r', '\n'], "_");
                let mut body = Vec::with_capacity(png.len() + 1_024);
                for (name, value) in &self.arguments {
                    body.extend_from_slice(
                        format!(
                            "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{}\r\n",
                            quote(name),
                            fill(value)
                        )
                        .as_bytes(),
                    );
                }
                body.extend_from_slice(
                    format!(
                        "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {mime}\r\n\r\n",
                        quote(&self.file_form_name),
                        quote(file_name)
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(png);
                body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
                headers.push((
                    "Content-Type".to_owned(),
                    format!("multipart/form-data; boundary={boundary}"),
                ));
                body
            }
        };
        HttpRequest {
            method: self.method.clone(),
            url,
            headers,
            body,
        }
    }

    /// Reads the public link from the response of the destination.
    pub(crate) fn result(
        &self,
        response: &HttpResponse,
        file_name: &str,
    ) -> Result<UploadResult, String> {
        let body = String::from_utf8_lossy(&response.body);
        let context = ResponseContext {
            body: body.trim(),
            json: Json::parse(body.trim()).ok(),
            headers: &response.headers,
            file_name,
        };
        if !(200..300).contains(&response.status) {
            let detail = context
                .fill(&self.error_message)
                .ok()
                .filter(|detail| !detail.is_empty())
                .unwrap_or_else(|| context.body.chars().take(200).collect());
            return Err(format!(
                "{} answered with HTTP status {}: {detail}",
                self.name, response.status
            ));
        }
        let url = if self.url.is_empty() {
            context.body.to_owned()
        } else {
            context.fill(&self.url)?
        };
        if !(url.starts_with("https://") || url.starts_with("http://"))
            || url.contains(char::is_whitespace)
        {
            return Err(format!(
                "{} did not return a link; the response rule gave: {}",
                self.name,
                url.chars().take(120).collect::<String>()
            ));
        }
        Ok(UploadResult {
            url,
            deletion_url: context
                .fill(&self.deletion_url)
                .ok()
                .filter(|link| link.starts_with("http")),
        })
    }
}

/// The response data that a link rule can refer to.
struct ResponseContext<'a> {
    body: &'a str,
    json: Option<Json>,
    headers: &'a [(String, String)],
    file_name: &'a str,
}

impl ResponseContext<'_> {
    /// Replaces each `{...}` expression of a rule with response data.
    fn fill(&self, rule: &str) -> Result<String, String> {
        let mut output = String::new();
        let mut rest = rule;
        while let Some(start) = rest.find('{') {
            output.push_str(&rest[..start]);
            let Some(length) = rest[start..].find('}') else {
                return Err(format!("the rule {rule} has an open brace"));
            };
            let expression = &rest[start + 1..start + length];
            rest = &rest[start + length + 1..];
            let (kind, argument) = expression.split_once(':').unwrap_or((expression, ""));
            let value = match kind.to_ascii_lowercase().as_str() {
                "json" => self
                    .json
                    .as_ref()
                    .and_then(|json| json.lookup(argument))
                    .and_then(Json::as_text)
                    .ok_or_else(|| format!("the response has no JSON value at {argument}"))?,
                "response" => self.body.to_owned(),
                "filename" => self.file_name.to_owned(),
                "header" => self
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(argument))
                    .map(|(_, value)| value.clone())
                    .ok_or_else(|| format!("the response has no {argument} header"))?,
                _ => return Err(format!("the expression {{{expression}}} is not supported")),
            };
            output.push_str(&value);
        }
        output.push_str(rest);
        Ok(output)
    }
}

/// Returns the folder that holds the settings of the user.
pub(crate) fn settings_directory() -> Option<PathBuf> {
    env::var_os("APPDATA").map(|data| PathBuf::from(data).join("record"))
}

/// Returns the folder that holds the `.sxcu` files of the user.
#[must_use]
pub fn uploaders_directory() -> Option<PathBuf> {
    settings_directory().map(|directory| directory.join("uploaders"))
}

/// Returns the destination named in `uploader.txt`, when the file exists.
#[must_use]
pub fn default_destination() -> Option<String> {
    let text = fs::read_to_string(settings_directory()?.join("uploader.txt")).ok()?;
    let name = text.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Checks the shape of an API key before it is stored.
fn valid_key(key: &str) -> bool {
    (6..=128).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Reads one small settings file. A missing file gives `None`.
#[must_use]
pub fn read_setting(file: &str) -> Option<String> {
    fs::read_to_string(settings_directory()?.join(file)).ok()
}

/// Writes one small settings file.
pub fn write_setting(file: &str, text: &str) -> Result<(), String> {
    let directory =
        settings_directory().ok_or_else(|| "the APPDATA folder is not available".to_owned())?;
    fs::create_dir_all(&directory)
        .and_then(|()| fs::write(directory.join(file), text))
        .map_err(|error| format!("could not write {file}: {error}"))
}

/// Stores the default destination. `None` clears it.
pub fn set_default_destination(destination: Option<&str>) -> Result<(), String> {
    write_setting("uploader.txt", destination.unwrap_or(""))
}

/// Lists the names of the `.sxcu` files in the uploaders folder.
#[must_use]
pub fn list_destinations() -> Vec<String> {
    let Some(entries) = uploaders_directory().and_then(|directory| fs::read_dir(directory).ok())
    else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("sxcu"))
        })
        .filter_map(|path| Some(path.file_stem()?.to_string_lossy().into_owned()))
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names
}

/// Checks a `.sxcu` file and copies it into the uploaders folder.
///
/// Returns the destination name that selects the copy.
pub fn import_sxcu(path: &Path) -> Result<String, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty() && Preset::find(stem).is_none())
        .ok_or_else(|| "the file name is the name of a built-in destination".to_owned())?;
    Uploader::from_sxcu(&text, &name)?;
    let directory =
        uploaders_directory().ok_or_else(|| "the APPDATA folder is not available".to_owned())?;
    fs::create_dir_all(&directory)
        .and_then(|()| fs::write(directory.join(format!("{name}.sxcu")), text))
        .map_err(|error| format!("could not store the uploader: {error}"))?;
    Ok(name)
}

/// Finds an upload destination by name or by `.sxcu` path.
///
/// The identifier of a built-in destination, such as `imgur` or `catbox`,
/// selects it. Any other value is a path to a `.sxcu` file or the name of a
/// file in the uploaders folder.
pub fn resolve_destination(destination: &str) -> Result<Uploader, String> {
    if let Some(preset) = Preset::find(destination) {
        let key = match preset.key_label {
            None => String::new(),
            Some(label) => preset.key().ok_or_else(|| {
                format!(
                    "{} needs its {label}. Open record screenshot, press P, and select {} \
                     to enter it",
                    preset.name, preset.name
                )
            })?,
        };
        return Uploader::preset(preset.id, &key)
            .ok_or_else(|| format!("{} is not available", preset.name));
    }
    let direct = Path::new(destination);
    let file = if direct.is_file() {
        direct.to_path_buf()
    } else {
        let directory = uploaders_directory()
            .ok_or_else(|| "the APPDATA folder is not available".to_owned())?;
        let named = directory.join(format!("{destination}.sxcu"));
        if !named.is_file() {
            return Err(format!(
                "no upload destination named {destination}; put {destination}.sxcu in {}",
                directory.display()
            ));
        }
        named
    };
    let text = fs::read_to_string(&file)
        .map_err(|error| format!("could not read {}: {error}", file.display()))?;
    let fallback = file.file_stem().map_or_else(
        || destination.to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    Uploader::from_sxcu(&text, &fallback).map_err(|error| format!("{}: {error}", file.display()))
}

/// Appends one finished upload to `uploads.log` so deletion links are kept.
pub(crate) fn log_upload(file_name: &str, result: &UploadResult) {
    use std::io::Write;

    let Some(directory) = settings_directory() else {
        return;
    };
    let _ = fs::create_dir_all(&directory);
    if let Ok(mut log) = fs::File::options()
        .create(true)
        .append(true)
        .open(directory.join("uploads.log"))
    {
        let _ = writeln!(
            log,
            "{file_name}\t{}\t{}",
            result.url,
            result.deletion_url.as_deref().unwrap_or("")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZIPLINE: &str = r#"{
        "Version": "17.0.0",
        "Name": "Zipline",
        "DestinationType": "ImageUploader, FileUploader",
        "RequestMethod": "post",
        "RequestURL": "https://files.example.com/api/upload",
        "Parameters": { "folder": "shots & more" },
        "Headers": { "authorization": "token-123", "Content-Type": "ignored" },
        "Body": "MultipartFormData",
        "Arguments": { "title": "{filename}" },
        "FileFormName": "file",
        "URL": "{json:files[0].url}",
        "DeletionURL": "https://files.example.com/delete/{json:files[0].id}",
        "ErrorMessage": "{json:error}"
    }"#;

    /// Builds a response with a JSON body.
    fn response(status: u32, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            headers: vec![("Location".to_owned(), "https://example.com/l".to_owned())],
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn json_reads_nested_values_and_escapes() {
        let json = Json::parse(r#" {"a": [1, 2.5e3, {"b": "x\né😀\"y"}], "t": true, "n": null} "#)
            .unwrap();
        assert_eq!(json.lookup("a[1]"), Some(&Json::Number("2.5e3".to_owned())));
        assert_eq!(
            json.lookup("a[2].b").and_then(Json::as_text).as_deref(),
            Some("x\né😀\"y")
        );
        assert_eq!(
            json.lookup("t").and_then(Json::as_text).as_deref(),
            Some("true")
        );
        assert_eq!(json.lookup("n"), Some(&Json::Null));
        assert_eq!(json.lookup("a[9]"), None);
        assert_eq!(json.lookup("a.b"), None);
    }

    #[test]
    fn json_rejects_broken_documents() {
        for text in [
            "",
            "{",
            r#"{"a" 1}"#,
            "[1 2]",
            r#""open"#,
            "nul",
            "{} x",
            "[1,]",
        ] {
            assert!(Json::parse(text).is_err(), "{text} should fail");
        }
        assert!(Json::parse(&"[".repeat(200)).is_err());
    }

    #[test]
    fn url_parts_cover_ports_queries_and_bad_hosts() {
        let parts = UrlParts::parse("HTTPS://Example.com/api/upload?x=1").unwrap();
        assert!(parts.secure);
        assert_eq!((parts.host.as_str(), parts.port), ("Example.com", 443));
        assert_eq!(parts.path, "/api/upload?x=1");
        let local = UrlParts::parse("http://localhost:8080").unwrap();
        assert_eq!(
            (local.secure, local.port, local.path.as_str()),
            (false, 8080, "/")
        );
        assert_eq!(UrlParts::parse("http://h?q=1").unwrap().path, "/?q=1");
        for url in [
            "ftp://h/x",
            "https://",
            "https://user@h/",
            "https://h:port/",
        ] {
            assert!(UrlParts::parse(url).is_err(), "{url} should fail");
        }
    }

    #[test]
    fn sxcu_builds_a_multipart_request() {
        let uploader = Uploader::from_sxcu(ZIPLINE, "fallback").unwrap();
        assert_eq!(uploader.name, "Zipline");
        let request = uploader.request("shot 1.png", b"PNGDATA");
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.url,
            "https://files.example.com/api/upload?folder=shots%20%26%20more"
        );
        assert_eq!(
            request.headers[0],
            ("authorization".to_owned(), "token-123".to_owned())
        );
        let (name, content_type) = &request.headers[1];
        assert_eq!(name, "Content-Type");
        let boundary = content_type.split("boundary=").nth(1).unwrap();
        let body = String::from_utf8(request.body).unwrap();
        assert!(body.starts_with(&format!("--{boundary}\r\n")));
        assert!(body.contains("name=\"title\"\r\n\r\nshot 1.png\r\n"));
        assert!(body.contains(
            "name=\"file\"; filename=\"shot 1.png\"\r\nContent-Type: image/png\r\n\r\nPNGDATA\r\n"
        ));
        assert!(body.ends_with(&format!("--{boundary}--\r\n")));
        assert_eq!(request.headers.len(), 2);
    }

    #[test]
    fn sxcu_reads_links_from_the_response() {
        let uploader = Uploader::from_sxcu(ZIPLINE, "fallback").unwrap();
        let result = uploader
            .result(
                &response(
                    200,
                    r#"{"files":[{"id":"abc","url":"https://files.example.com/u/abc.png"}]}"#,
                ),
                "shot.png",
            )
            .unwrap();
        assert_eq!(result.url, "https://files.example.com/u/abc.png");
        assert_eq!(
            result.deletion_url.as_deref(),
            Some("https://files.example.com/delete/abc")
        );
    }

    #[test]
    fn failures_use_the_error_rule_and_the_status() {
        let uploader = Uploader::from_sxcu(ZIPLINE, "fallback").unwrap();
        let error = uploader
            .result(&response(401, r#"{"error":"bad token"}"#), "shot.png")
            .unwrap_err();
        assert_eq!(error, "Zipline answered with HTTP status 401: bad token");
        let missing = uploader
            .result(&response(200, r#"{"files":[]}"#), "shot.png")
            .unwrap_err();
        assert!(missing.contains("files[0].url"), "{missing}");
        let html = uploader.result(&response(502, "<html>bad gateway</html>"), "shot.png");
        assert!(html.unwrap_err().contains("502: <html>bad gateway"));
    }

    #[test]
    fn a_binary_uploader_sends_raw_bytes_and_accepts_a_plain_link() {
        let uploader = Uploader::from_sxcu(
            r#"{"RequestURL":"https://paste.example.com/","RequestMethod":"PUT","Body":"Binary"}"#,
            "paste",
        )
        .unwrap();
        assert_eq!(uploader.name, "paste");
        let request = uploader.request("a.png", b"RAW");
        assert_eq!(
            (request.method.as_str(), request.body.as_slice()),
            ("PUT", &b"RAW"[..])
        );
        assert_eq!(
            request.headers,
            [("Content-Type".to_owned(), "image/png".to_owned())]
        );
        let result = uploader
            .result(
                &response(201, " https://paste.example.com/x.png\n"),
                "a.png",
            )
            .unwrap();
        assert_eq!(result.url, "https://paste.example.com/x.png");
        assert_eq!(result.deletion_url, None);
        assert!(uploader.result(&response(200, "ok"), "a.png").is_err());
    }

    #[test]
    fn header_and_response_expressions_work_and_unknown_ones_fail() {
        let mut uploader = Uploader::from_sxcu(ZIPLINE, "fallback").unwrap();
        uploader.url = "{header:location}/{filename}".to_owned();
        let result = uploader.result(&response(200, "{}"), "a.png").unwrap();
        assert_eq!(result.url, "https://example.com/l/a.png");
        uploader.url = "{regex:x}".to_owned();
        assert!(
            uploader
                .result(&response(200, "{}"), "a.png")
                .unwrap_err()
                .contains("not supported")
        );
    }

    #[test]
    fn unusable_files_are_rejected_with_a_reason() {
        let json_body = r#"{"RequestURL":"https://x.example/","Body":"JSON"}"#;
        assert!(
            Uploader::from_sxcu(json_body, "x")
                .unwrap_err()
                .contains("json")
        );
        assert!(
            Uploader::from_sxcu("{}", "x")
                .unwrap_err()
                .contains("RequestURL")
        );
        assert!(Uploader::from_sxcu(r#"{"RequestURL":"ftp://x/"}"#, "x").is_err());
        assert!(Uploader::from_sxcu("not json", "x").is_err());
    }

    #[test]
    fn keys_are_checked_before_they_are_stored() {
        assert!(valid_key("0123456789abcde"));
        assert!(valid_key("abc-DEF_123"));
        for key in ["", "short", "has space 123", "semi;colon123"] {
            assert!(!valid_key(key), "{key} should fail");
        }
    }

    #[test]
    fn every_preset_builds_a_request_for_its_service() {
        for preset in PRESETS {
            let uploader = Uploader::preset(preset.id, "key123456").unwrap();
            assert_eq!(uploader.name, preset.name);
            let request = uploader.request("a.png", b"P");
            assert!(request.url.starts_with("https://"), "{}", request.url);
            assert!(UrlParts::parse(&request.url).is_ok());
        }
        assert_eq!(
            Preset::find("CATBOX").map(|preset| preset.name),
            Some("Catbox")
        );
        assert!(Uploader::preset("nowhere", "").is_none());
    }

    #[test]
    fn imgbb_sends_the_key_in_the_address_and_reads_both_links() {
        let imgbb = Uploader::preset("imgbb", "abc123def").unwrap();
        let request = imgbb.request("a.png", b"P");
        assert_eq!(request.url, "https://api.imgbb.com/1/upload?key=abc123def");
        assert!(String::from_utf8_lossy(&request.body).contains("name=\"image\"; filename"));
        let body = r#"{"data":{"url":"https://i.ibb.co/x/a.png","delete_url":"https://ibb.co/x/d"},"success":true,"status":200}"#;
        let result = imgbb.result(&response(200, body), "a.png").unwrap();
        assert_eq!(result.url, "https://i.ibb.co/x/a.png");
        assert_eq!(result.deletion_url.as_deref(), Some("https://ibb.co/x/d"));
        let denied = r#"{"status_code":400,"error":{"message":"Invalid API v1 key.","code":100}}"#;
        let error = imgbb.result(&response(400, denied), "a.png").unwrap_err();
        assert_eq!(
            error,
            "ImgBB answered with HTTP status 400: Invalid API v1 key."
        );
    }

    #[test]
    fn catbox_and_litterbox_send_their_form_fields_and_read_a_plain_link() {
        let catbox = Uploader::preset("catbox", "").unwrap();
        let body = String::from_utf8_lossy(&catbox.request("a.png", b"P").body).into_owned();
        assert!(body.contains("name=\"reqtype\"\r\n\r\nfileupload\r\n"));
        assert!(body.contains("name=\"fileToUpload\"; filename=\"a.png\""));
        let result = catbox
            .result(
                &response(200, "https://files.catbox.moe/abc123.png"),
                "a.png",
            )
            .unwrap();
        assert_eq!(result.url, "https://files.catbox.moe/abc123.png");
        let litter = Uploader::preset("litterbox", "").unwrap();
        let body = String::from_utf8_lossy(&litter.request("a.png", b"P").body).into_owned();
        assert!(body.contains("name=\"time\"\r\n\r\n72h\r\n"));
        assert!(
            catbox
                .result(&response(200, "No request type given."), "a.png")
                .is_err()
        );
    }

    #[test]
    fn the_imgur_destination_matches_the_imgur_api() {
        let imgur = Uploader::imgur(" abc123 \n");
        let request = imgur.request("a.png", b"P");
        assert_eq!(request.url, "https://api.imgur.com/3/image");
        assert_eq!(request.headers[0].1, "Client-ID abc123");
        assert!(String::from_utf8_lossy(&request.body).contains("name=\"image\""));
        let result = imgur
            .result(
                &response(200, r#"{"data":{"link":"https://i.imgur.com/x.png","deletehash":"dh"},"success":true}"#),
                "a.png",
            )
            .unwrap();
        assert_eq!(result.url, "https://i.imgur.com/x.png");
        assert_eq!(
            result.deletion_url.as_deref(),
            Some("https://imgur.com/delete/dh")
        );
    }
}
