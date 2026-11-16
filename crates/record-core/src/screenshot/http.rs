//! A small HTTP client on WinHTTP for upload requests.
//!
//! WinHTTP is part of Windows. It uses the system proxy settings and the
//! system certificate store, and it adds no code to the executable.

use std::{ffi::c_void, ptr};

use anyhow::{Context, Result, anyhow, bail};
use windows::{
    Win32::Networking::WinHttp::{
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
        WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_RAW_HEADERS_CRLF, WINHTTP_QUERY_STATUS_CODE,
        WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
        WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    },
    core::{HSTRING, PCWSTR, w},
};

use super::upload::{HttpRequest, HttpResponse, UrlParts};

/// Largest response body that the client keeps.
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;

/// Closes one WinHTTP handle when it leaves scope.
struct Handle(*mut c_void);

impl Handle {
    /// Wraps a handle and reports a null handle as the last system error.
    fn new(handle: *mut c_void, action: &str) -> Result<Self> {
        if handle.is_null() {
            return Err(anyhow!(windows::core::Error::from_thread()).context(action.to_owned()));
        }
        Ok(Self(handle))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: The handle came from WinHTTP and is closed exactly once.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// Sends one request and returns the complete response.
pub(crate) fn send(request: &HttpRequest) -> Result<HttpResponse> {
    let parts = UrlParts::parse(&request.url).map_err(|error| anyhow!(error))?;
    let body_length =
        u32::try_from(request.body.len()).context("the upload is larger than 4 GB")?;
    let host = HSTRING::from(parts.host.as_str());
    let path = HSTRING::from(parts.path.as_str());
    let method = HSTRING::from(request.method.as_str());
    let header_text: String = request
        .headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    let header_wide: Vec<u16> = header_text.encode_utf16().collect();
    // SAFETY: Every string outlives the call that reads it. Each handle is
    // owned by a guard and used only while its parent handle is open. Buffer
    // pointers and lengths describe live local buffers.
    unsafe {
        let session = Handle::new(
            WinHttpOpen(
                w!("record-screenshot"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "could not start WinHTTP",
        )?;
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 60_000, 60_000)
            .context("could not set the network timeouts")?;
        let connection = Handle::new(
            WinHttpConnect(session.0, &host, parts.port, 0),
            "could not open the connection",
        )?;
        let flags = if parts.secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
        let handle = Handle::new(
            WinHttpOpenRequest(
                connection.0,
                &method,
                &path,
                PCWSTR::null(),
                PCWSTR::null(),
                ptr::null(),
                flags,
            ),
            "could not create the request",
        )?;
        WinHttpSendRequest(
            handle.0,
            (!header_wide.is_empty()).then_some(header_wide.as_slice()),
            Some(request.body.as_ptr().cast()),
            body_length,
            body_length,
            0,
        )
        .with_context(|| format!("could not reach {}", parts.host))?;
        WinHttpReceiveResponse(handle.0, ptr::null_mut())
            .with_context(|| format!("{} did not answer", parts.host))?;

        let mut status = 0_u32;
        let mut status_size = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            handle.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(ptr::from_mut(&mut status).cast()),
            &mut status_size,
            ptr::null_mut(),
        )
        .context("the response has no status code")?;

        // The first call reports the needed size in bytes and fails.
        let mut header_bytes = 0_u32;
        let _ = WinHttpQueryHeaders(
            handle.0,
            WINHTTP_QUERY_RAW_HEADERS_CRLF,
            PCWSTR::null(),
            None,
            &mut header_bytes,
            ptr::null_mut(),
        );
        let mut raw_headers = vec![0_u16; header_bytes as usize / 2 + 1];
        let mut headers = Vec::new();
        if WinHttpQueryHeaders(
            handle.0,
            WINHTTP_QUERY_RAW_HEADERS_CRLF,
            PCWSTR::null(),
            Some(raw_headers.as_mut_ptr().cast()),
            &mut header_bytes,
            ptr::null_mut(),
        )
        .is_ok()
        {
            let text = String::from_utf16_lossy(&raw_headers[..header_bytes as usize / 2]);
            headers = text
                .lines()
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
                .collect();
        }

        let mut body = Vec::new();
        let mut chunk = vec![0_u8; 16 * 1024];
        loop {
            let mut read = 0_u32;
            WinHttpReadData(
                handle.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            )
            .context("the response ended early")?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > RESPONSE_LIMIT {
                bail!("the response is larger than 4 MB");
            }
        }
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}
