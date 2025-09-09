// MIT License Copyright (c) 2022 Blobscan <https://blobscan.com>
//
// Permission is hereby granted, free of charge,
// to any person obtaining a copy of this software and associated documentation
// files (the "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish, distribute,
// sublicense, and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above
// copyright notice and this permission notice (including the next paragraph) shall
// be included in all copies or substantial portions of the Software.
//
// THE SOFTWARE
// IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING
// BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
// PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS
// BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
// CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
// SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

#[macro_export]
/// Make a GET request sending and expecting JSON.
/// if JSON deser fails, emit a `WARN` level tracing event
macro_rules! json_get {
    ($client:expr, $url:expr, $expected:ty, $exp_backoff:expr) => {
        json_get!($client, $url, $expected, "", $exp_backoff)
    };
    ($client:expr, $url:expr, $expected:ty, $auth_token:expr, $exp_backoff: expr) => {{
        let url = $url.clone();

        tracing::trace!(method = "GET", url = url.as_str(), "Dispatching API request");

        let mut req = $client.get($url);

        if !$auth_token.is_empty() {
          req = req.bearer_auth($auth_token);
        }

        let resp = if $exp_backoff.is_some() {
            match backoff::future::retry_notify(
                $exp_backoff.unwrap(),
                || {
                    let req = req.try_clone().unwrap();

                    async move { req.send().await.map_err(|err| err.into()) }
                },
                |error, duration: std::time::Duration| {
                    let duration = duration.as_secs();

                    tracing::warn!(
                        method = "GET",
                        url = %url,
                        ?error,
                        "Failed to send request. Retrying in {duration} seconds…"
                    );
                },
            )
            .await {
                Ok(resp) => resp,
                Err(error) => {
                    tracing::warn!(
                        method = "GET",
                        url = %url,
                        ?error,
                        "Failed to send request. All retries failed"
                    );

                    return Err(error.into())
                }
            }
        } else {
            match req.send().await {
                Err(error) => {
                    tracing::warn!(
                        method = "GET",
                        url = %url,
                        ?error,
                        "Failed to send request"
                    );

                    return Err(error.into())
                },
                Ok(resp) => resp
            }
        };

        let status = resp.status();

        if status.as_u16() == 404 {
          return Ok(None)
        };

        let text = resp.text().await?;
        let result: Result<$crate::clients::common::ClientResponse<$expected>, _> = serde_json::from_str(&text);

        match result {
            Err(e) => {
                tracing::warn!(
                    method = "GET",
                    url = %url,
                    response = text.as_str(),
                    "Unexpected response from server"
                );

                Err(e.into())
            },
            Ok(response) => {
              response.into_client_result()
            }
        }
    }};
}

#[macro_export]
/// Make a PUT request sending JSON.
/// if JSON deser fails, emit a `WARN` level tracing event
macro_rules! json_put {
    ($client:expr, $url:expr, $auth_token:expr, $body:expr) => {
        json_put!($client, $url, (), $auth_token, $body)
    };
    ($client:expr, $url:expr, $expected:ty, $auth_token:expr, $body:expr) => {{
        let url = $url.clone();
        let body = format!("{:?}", $body);

        tracing::trace!(method = "PUT", url = url.as_str(), body, "Dispatching API client request");


        let resp = match $client
            .put($url)
            .bearer_auth($auth_token)
            .json($body)
            .send()
            .await {
                Err(error) => {
                    tracing::warn!(
                        method = "PUT",
                        url = %url,
                        body = body,
                        ?error,
                        "Failed to send request"
                    );

                    return Err(error.into())
                },
                Ok(resp) => resp
            };

        let text = resp.text().await?;
        let result: $crate::clients::common::ClientResponse<$expected> = text.parse()?;

        if result.is_err() {
            tracing::warn!(
                method = "PUT",
                url = %url,
                body,
                response = text.as_str(),
                "Unexpected response from server"
            );
        }

        result.into_client_result()
    }};
}
