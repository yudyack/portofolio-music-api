use crate::config::Config;
use url::Url;

/// Minimum OAuth scopes for the music-api features (spec §5.3). Exposed so
/// tests can assert exact-membership and the architect can grep for one
/// authoritative list. Adding a scope here is a spec change, not a code
/// change — coordinate with the specifier.
pub const SCOPES: &[&str] = &[
    "user-read-playback-state",
    "user-read-recently-played",
    "user-top-read",
    "user-read-private",
    "playlist-read-private",
    "user-follow-read",
];

const AUTHORIZE_ENDPOINT: &str = "https://accounts.spotify.com/authorize";

/// Build the Spotify authorize URL for the Authorization Code flow.
/// Always emits the `code` response type. Spec criterion 16 forbids the
/// Implicit Grant variant and there is no branch in this function that
/// can produce one — both the URL-builder unit test and a static-grep
/// test over src/ enforce this.
pub fn build_authorize_url(config: &Config, state: &str) -> String {
    let mut url = Url::parse(AUTHORIZE_ENDPOINT).expect("AUTHORIZE_ENDPOINT is a constant");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.spotify_client_id)
        .append_pair("redirect_uri", &config.spotify_redirect_uri)
        .append_pair("scope", &SCOPES.join(" "))
        .append_pair("state", state);
    url.into()
}
