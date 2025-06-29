use std::sync::{Arc, Mutex};

use language_tags::LanguageTag;
use matrix_sdk::widget::{MessageLikeEventFilter, StateEventFilter, ToDeviceEventFilter};
use matrix_sdk_common::{SendOutsideWasm, SyncOutsideWasm};
use ruma::events::MessageLikeEventType;
use tracing::error;

use crate::{room::Room, runtime::get_runtime_handle};

#[derive(uniffi::Record)]
pub struct WidgetDriverAndHandle {
    pub driver: Arc<WidgetDriver>,
    pub handle: Arc<WidgetDriverHandle>,
}

#[matrix_sdk_ffi_macros::export]
pub fn make_widget_driver(settings: WidgetSettings) -> Result<WidgetDriverAndHandle, ParseError> {
    let (driver, handle) = matrix_sdk::widget::WidgetDriver::new(settings.try_into()?);
    Ok(WidgetDriverAndHandle {
        driver: Arc::new(WidgetDriver(Mutex::new(Some(driver)))),
        handle: Arc::new(WidgetDriverHandle(handle)),
    })
}

/// An object that handles all interactions of a widget living inside a webview
/// or IFrame with the Matrix world.
#[derive(uniffi::Object)]
pub struct WidgetDriver(Mutex<Option<matrix_sdk::widget::WidgetDriver>>);

#[matrix_sdk_ffi_macros::export]
impl WidgetDriver {
    pub async fn run(
        &self,
        room: Arc<Room>,
        capabilities_provider: Box<dyn WidgetCapabilitiesProvider>,
    ) {
        let Some(driver) = self.0.lock().unwrap().take() else {
            error!("Can't call run multiple times on a WidgetDriver");
            return;
        };

        let capabilities_provider = CapabilitiesProviderWrap(capabilities_provider.into());
        if let Err(()) = driver.run(room.inner.clone(), capabilities_provider).await {
            // TODO
        }
    }
}

/// Information about a widget.
#[derive(uniffi::Record, Clone)]
pub struct WidgetSettings {
    /// Widget's unique identifier.
    pub widget_id: String,
    /// Whether or not the widget should be initialized on load message
    /// (`ContentLoad` message), or upon creation/attaching of the widget to
    /// the SDK's state machine that drives the API.
    pub init_after_content_load: bool,
    /// This contains the url from the widget state event.
    /// In this url placeholders can be used to pass information from the client
    /// to the widget. Possible values are: `$widgetId`, `$parentUrl`,
    /// `$userId`, `$lang`, `$fontScale`, `$analyticsID`.
    ///
    /// # Examples
    ///
    /// e.g `http://widget.domain?username=$userId`
    /// will become: `http://widget.domain?username=@user_matrix_id:server.domain`.
    raw_url: String,
}

impl TryFrom<WidgetSettings> for matrix_sdk::widget::WidgetSettings {
    type Error = ParseError;

    fn try_from(value: WidgetSettings) -> Result<Self, Self::Error> {
        let WidgetSettings { widget_id, init_after_content_load, raw_url } = value;
        Ok(matrix_sdk::widget::WidgetSettings::new(widget_id, init_after_content_load, &raw_url)?)
    }
}

impl From<matrix_sdk::widget::WidgetSettings> for WidgetSettings {
    fn from(value: matrix_sdk::widget::WidgetSettings) -> Self {
        WidgetSettings {
            widget_id: value.widget_id().to_owned(),
            init_after_content_load: value.init_on_content_load(),
            raw_url: value.raw_url().to_string(),
        }
    }
}

/// Create the actual url that can be used to setup the WebView or IFrame
/// that contains the widget.
///
/// # Arguments
/// * `widget_settings` - The widget settings to generate the url for.
/// * `room` - A Matrix room which is used to query the logged in username
/// * `props` - Properties from the client that can be used by a widget to adapt
///   to the client. e.g. language, font-scale...
#[matrix_sdk_ffi_macros::export]
pub async fn generate_webview_url(
    widget_settings: WidgetSettings,
    room: Arc<Room>,
    props: ClientProperties,
) -> Result<String, ParseError> {
    Ok(matrix_sdk::widget::WidgetSettings::generate_webview_url(
        &widget_settings.clone().try_into()?,
        &room.inner,
        props.into(),
    )
    .await
    .map(|url| url.to_string())?)
}

/// Defines if a call is encrypted and which encryption system should be used.
///
/// This controls the url parameters: `perParticipantE2EE`, `password`.
#[derive(uniffi::Enum, Clone)]
pub enum EncryptionSystem {
    /// Equivalent to the element call url parameter: `enableE2EE=false`
    Unencrypted,
    /// Equivalent to the element call url parameter:
    /// `perParticipantE2EE=true`
    PerParticipantKeys,
    /// Equivalent to the element call url parameter:
    /// `password={secret}`
    SharedSecret {
        /// The secret/password which is used in the url.
        secret: String,
    },
}

impl From<EncryptionSystem> for matrix_sdk::widget::EncryptionSystem {
    fn from(value: EncryptionSystem) -> Self {
        match value {
            EncryptionSystem::Unencrypted => Self::Unencrypted,
            EncryptionSystem::PerParticipantKeys => Self::PerParticipantKeys,
            EncryptionSystem::SharedSecret { secret } => Self::SharedSecret { secret },
        }
    }
}

/// Defines the intent of showing the call.
///
/// This controls whether to show or skip the lobby.
#[derive(uniffi::Enum, Clone)]
pub enum Intent {
    /// The user wants to start a call.
    StartCall,
    /// The user wants to join an existing call.
    JoinExisting,
}
impl From<Intent> for matrix_sdk::widget::Intent {
    fn from(value: Intent) -> Self {
        match value {
            Intent::StartCall => Self::StartCall,
            Intent::JoinExisting => Self::JoinExisting,
        }
    }
}

/// Properties to create a new virtual Element Call widget.
#[derive(uniffi::Record, Clone)]
pub struct VirtualElementCallWidgetOptions {
    /// The url to the Element Call app including any `/room` path if required.
    ///
    /// E.g. <https://call.element.io>, <https://call.element.dev>, <https://call.element.dev/room>
    pub element_call_url: String,

    /// The widget id.
    pub widget_id: String,

    /// The url that is used as the target for the PostMessages sent
    /// by the widget (to the client).
    ///
    /// For a web app client this is the client url. In case of using other
    /// platforms the client most likely is setup up to listen to
    /// postmessages in the same webview the widget is hosted. In this case
    /// the `parent_url` is set to the url of the webview with the widget. Be
    /// aware that this means that the widget will receive its own postmessage
    /// messages. The `matrix-widget-api` (js) ignores those so this works but
    /// it might break custom implementations.
    ///
    /// Defaults to `element_call_url` for the non-iframe (dedicated webview)
    /// usecase.
    pub parent_url: Option<String>,

    /// Whether the branding header of Element call should be hidden.
    ///
    /// Default: `true`
    pub hide_header: Option<bool>,

    /// If set, the lobby will be skipped and the widget will join the
    /// call on the `io.element.join` action.
    ///
    /// Default: `false`
    pub preload: Option<bool>,

    /// The font scale which will be used inside element call.
    ///
    /// Default: `1`
    pub font_scale: Option<f64>,

    /// Whether element call should prompt the user to open in the browser or
    /// the app.
    ///
    /// Default: `false`
    pub app_prompt: Option<bool>,

    /// Make it not possible to get to the calls list in the webview.
    ///
    /// Default: `true`
    pub confine_to_room: Option<bool>,

    /// The font to use, to adapt to the system font.
    pub font: Option<String>,

    /// The encryption system to use.
    ///
    /// Use `EncryptionSystem::Unencrypted` to disable encryption.
    pub encryption: EncryptionSystem,

    /// The intent of showing the call.
    /// If the user wants to start a call or join an existing one.
    /// Controls if the lobby is skipped or not.
    pub intent: Option<Intent>,

    /// Do not show the screenshare button.
    pub hide_screensharing: bool,

    /// Can be used to pass a PostHog id to element call.
    pub posthog_user_id: Option<String>,
    /// The host of the posthog api.
    /// Supported since Element Call v0.9.0. Only used by the embedded package.
    pub posthog_api_host: Option<String>,
    /// The key for the posthog api.
    /// Supported since Element Call v0.9.0. Only used by the embedded package.
    pub posthog_api_key: Option<String>,

    /// The url to use for submitting rageshakes.
    /// Supported since Element Call v0.9.0. Only used by the embedded package.
    pub rageshake_submit_url: Option<String>,

    /// Sentry [DSN](https://docs.sentry.io/concepts/key-terms/dsn-explainer/)
    /// Supported since Element Call v0.9.0. Only used by the embedded package.
    pub sentry_dsn: Option<String>,
    /// Sentry [environment](https://docs.sentry.io/concepts/key-terms/key-terms/)
    /// Supported since Element Call v0.9.0. Only used by the embedded package.
    pub sentry_environment: Option<String>,
    //// - `true`: The webview should show the list of media devices it detects using
    ////   `enumerateDevices`.
    ///  - `false`: the webview shows a a list of devices injected by the
    ///    client. (used on ios & android)
    pub controlled_media_devices: bool,
}

impl From<VirtualElementCallWidgetOptions> for matrix_sdk::widget::VirtualElementCallWidgetOptions {
    fn from(value: VirtualElementCallWidgetOptions) -> Self {
        Self {
            element_call_url: value.element_call_url,
            widget_id: value.widget_id,
            parent_url: value.parent_url,
            hide_header: value.hide_header,
            preload: value.preload,
            font_scale: value.font_scale,
            app_prompt: value.app_prompt,
            confine_to_room: value.confine_to_room,
            font: value.font,
            posthog_user_id: value.posthog_user_id,
            encryption: value.encryption.into(),
            intent: value.intent.map(Into::into),
            hide_screensharing: value.hide_screensharing,
            posthog_api_host: value.posthog_api_host,
            posthog_api_key: value.posthog_api_key,
            rageshake_submit_url: value.rageshake_submit_url,
            sentry_dsn: value.sentry_dsn,
            sentry_environment: value.sentry_environment,
            controlled_media_devices: value.controlled_media_devices,
        }
    }
}

/// `WidgetSettings` are usually created from a state event.
/// (currently unimplemented)
///
/// In some cases the client wants to create custom `WidgetSettings`
/// for specific rooms based on other conditions.
/// This function returns a `WidgetSettings` object which can be used
/// to setup a widget using `run_client_widget_api`
/// and to generate the correct url for the widget.
///
/// # Arguments
///
/// * `props` - A struct containing the configuration parameters for a element
///   call widget.
#[matrix_sdk_ffi_macros::export]
pub fn new_virtual_element_call_widget(
    props: VirtualElementCallWidgetOptions,
) -> Result<WidgetSettings, ParseError> {
    Ok(matrix_sdk::widget::WidgetSettings::new_virtual_element_call_widget(props.into())
        .map(|w| w.into())?)
}

/// The Capabilities required to run a element call widget.
///
/// This is intended to be used in combination with: `acquire_capabilities` of
/// the `CapabilitiesProvider`.
///
/// `acquire_capabilities` can simply return the `WidgetCapabilities` from this
/// function. Even if there are non intersecting permissions to what the widget
/// requested.
///
/// Editing and extending the capabilities from this function is also possible,
/// but should only be done as temporal workarounds until this function is
/// adjusted
#[matrix_sdk_ffi_macros::export]
pub fn get_element_call_required_permissions(
    own_user_id: String,
    own_device_id: String,
) -> WidgetCapabilities {
    use ruma::events::StateEventType;

    let read_send = vec![
        // To read and send rageshake requests from other room members
        WidgetEventFilter::MessageLikeWithType {
            event_type: "org.matrix.rageshake_request".to_owned(),
        },
        // To read and send encryption keys
        WidgetEventFilter::ToDevice { event_type: "io.element.call.encryption_keys".to_owned() },
        // TODO change this to the appropriate to-device version once ready
        // remove this once all matrixRTC call apps supports to-device encryption.
        WidgetEventFilter::MessageLikeWithType {
            event_type: "io.element.call.encryption_keys".to_owned(),
        },
        // To read and send custom EC reactions. They are different to normal `m.reaction`
        // because they can be send multiple times to the same event.
        WidgetEventFilter::MessageLikeWithType {
            event_type: "io.element.call.reaction".to_owned(),
        },
        // This allows send raise hand reactions.
        WidgetEventFilter::MessageLikeWithType {
            event_type: MessageLikeEventType::Reaction.to_string(),
        },
        // This allows to detect if someone does not raise their hand anymore.
        WidgetEventFilter::MessageLikeWithType {
            event_type: MessageLikeEventType::RoomRedaction.to_string(),
        },
    ];

    WidgetCapabilities {
        read: vec![
            // To compute the current state of the matrixRTC session.
            WidgetEventFilter::StateWithType { event_type: StateEventType::CallMember.to_string() },
            // To detect leaving/kicked room members during a call.
            WidgetEventFilter::StateWithType { event_type: StateEventType::RoomMember.to_string() },
            // To decide whether to encrypt the call streams based on the room encryption setting.
            WidgetEventFilter::StateWithType {
                event_type: StateEventType::RoomEncryption.to_string(),
            },
            // This allows the widget to check the room version, so it can know about
            // version-specific auth rules (namely MSC3779).
            WidgetEventFilter::StateWithType { event_type: StateEventType::RoomCreate.to_string() },
        ]
        .into_iter()
        .chain(read_send.clone())
        .collect(),
        send: vec![
            // To send the call participation state event (main MatrixRTC event).
            // This is required for legacy state events (using only one event for all devices with
            // a membership array). TODO: remove once legacy call member events are
            // sunset.
            WidgetEventFilter::StateWithTypeAndStateKey {
                event_type: StateEventType::CallMember.to_string(),
                state_key: own_user_id.clone(),
            },
            // `delayed_event`` version for session memberhips
            // [MSC3779](https://github.com/matrix-org/matrix-spec-proposals/pull/3779), with no leading underscore.
            WidgetEventFilter::StateWithTypeAndStateKey {
                event_type: StateEventType::CallMember.to_string(),
                state_key: format!("{own_user_id}_{own_device_id}"),
            },
            // The same as above but with an underscore.
            // To work around the issue that state events starting with `@` have to be Matrix id's
            // but we use mxId+deviceId.
            WidgetEventFilter::StateWithTypeAndStateKey {
                event_type: StateEventType::CallMember.to_string(),
                state_key: format!("_{own_user_id}_{own_device_id}"),
            },
        ]
        .into_iter()
        .chain(read_send)
        .collect(),
        requires_client: true,
        update_delayed_event: true,
        send_delayed_event: true,
    }
}

#[derive(uniffi::Record)]
pub struct ClientProperties {
    /// The client_id provides the widget with the option to behave differently
    /// for different clients. e.g org.example.ios.
    client_id: String,
    /// The language tag the client is set to e.g. en-us. (Undefined and invalid
    /// becomes: `en-US`)
    language_tag: Option<String>,
    /// A string describing the theme (dark, light) or org.example.dark.
    /// (default: `light`)
    theme: Option<String>,
}

impl From<ClientProperties> for matrix_sdk::widget::ClientProperties {
    fn from(value: ClientProperties) -> Self {
        let ClientProperties { client_id, language_tag, theme } = value;
        let language_tag = language_tag.and_then(|l| LanguageTag::parse(&l).ok());
        Self::new(&client_id, language_tag, theme)
    }
}

/// A handle that encapsulates the communication between a widget driver and the
/// corresponding widget (inside a webview or IFrame).
#[derive(uniffi::Object)]
pub struct WidgetDriverHandle(matrix_sdk::widget::WidgetDriverHandle);

#[matrix_sdk_ffi_macros::export]
impl WidgetDriverHandle {
    /// Receive a message from the widget driver.
    ///
    /// The message must be passed on to the widget.
    ///
    /// Returns `None` if the widget driver is no longer running.
    pub async fn recv(&self) -> Option<String> {
        self.0.recv().await
    }

    //// Send a message from the widget to the widget driver.
    ///
    /// Returns `false` if the widget driver is no longer running.
    pub async fn send(&self, msg: String) -> bool {
        self.0.send(msg).await
    }
}

/// Capabilities that a widget can request from a client.
#[derive(uniffi::Record)]
pub struct WidgetCapabilities {
    /// Types of the messages that a widget wants to be able to fetch.
    pub read: Vec<WidgetEventFilter>,
    /// Types of the messages that a widget wants to be able to send.
    pub send: Vec<WidgetEventFilter>,
    /// If this capability is requested by the widget, it can not operate
    /// separately from the Matrix client.
    ///
    /// This means clients should not offer to open the widget in a separate
    /// browser/tab/webview that is not connected to the postmessage widget-api.
    pub requires_client: bool,
    /// This allows the widget to ask the client to update delayed events.
    pub update_delayed_event: bool,
    /// This allows the widget to send events with a delay.
    pub send_delayed_event: bool,
}

impl From<WidgetCapabilities> for matrix_sdk::widget::Capabilities {
    fn from(value: WidgetCapabilities) -> Self {
        Self {
            read: value.read.into_iter().map(Into::into).collect(),
            send: value.send.into_iter().map(Into::into).collect(),
            requires_client: value.requires_client,
            update_delayed_event: value.update_delayed_event,
            send_delayed_event: value.send_delayed_event,
        }
    }
}

impl From<matrix_sdk::widget::Capabilities> for WidgetCapabilities {
    fn from(value: matrix_sdk::widget::Capabilities) -> Self {
        Self {
            read: value.read.into_iter().map(Into::into).collect(),
            send: value.send.into_iter().map(Into::into).collect(),
            requires_client: value.requires_client,
            update_delayed_event: value.update_delayed_event,
            send_delayed_event: value.send_delayed_event,
        }
    }
}

/// Different kinds of filters that could be applied to the timeline events.
#[derive(uniffi::Enum, Clone)]
pub enum WidgetEventFilter {
    /// Matches message-like events with the given `type`.
    MessageLikeWithType { event_type: String },
    /// Matches `m.room.message` events with the given `msgtype`.
    RoomMessageWithMsgtype { msgtype: String },
    /// Matches state events with the given `type`, regardless of `state_key`.
    StateWithType { event_type: String },
    /// Matches state events with the given `type` and `state_key`.
    StateWithTypeAndStateKey { event_type: String, state_key: String },
    /// Matches to-device events with the given `event_type`.
    ToDevice { event_type: String },
}

impl From<WidgetEventFilter> for matrix_sdk::widget::Filter {
    fn from(value: WidgetEventFilter) -> Self {
        match value {
            WidgetEventFilter::MessageLikeWithType { event_type } => {
                Self::MessageLike(MessageLikeEventFilter::WithType(event_type.into()))
            }
            WidgetEventFilter::RoomMessageWithMsgtype { msgtype } => {
                Self::MessageLike(MessageLikeEventFilter::RoomMessageWithMsgtype(msgtype))
            }
            WidgetEventFilter::StateWithType { event_type } => {
                Self::State(StateEventFilter::WithType(event_type.into()))
            }
            WidgetEventFilter::StateWithTypeAndStateKey { event_type, state_key } => {
                Self::State(StateEventFilter::WithTypeAndStateKey(event_type.into(), state_key))
            }
            WidgetEventFilter::ToDevice { event_type } => {
                Self::ToDevice(ToDeviceEventFilter { event_type: event_type.into() })
            }
        }
    }
}

impl From<matrix_sdk::widget::Filter> for WidgetEventFilter {
    fn from(value: matrix_sdk::widget::Filter) -> Self {
        use matrix_sdk::widget::Filter as F;

        match value {
            F::MessageLike(MessageLikeEventFilter::WithType(event_type)) => {
                Self::MessageLikeWithType { event_type: event_type.to_string() }
            }
            F::MessageLike(MessageLikeEventFilter::RoomMessageWithMsgtype(msgtype)) => {
                Self::RoomMessageWithMsgtype { msgtype }
            }
            F::State(StateEventFilter::WithType(event_type)) => {
                Self::StateWithType { event_type: event_type.to_string() }
            }
            F::State(StateEventFilter::WithTypeAndStateKey(event_type, state_key)) => {
                Self::StateWithTypeAndStateKey { event_type: event_type.to_string(), state_key }
            }
            F::ToDevice(ToDeviceEventFilter { event_type }) => {
                Self::ToDevice { event_type: event_type.to_string() }
            }
        }
    }
}

#[matrix_sdk_ffi_macros::export(callback_interface)]
pub trait WidgetCapabilitiesProvider: SendOutsideWasm + SyncOutsideWasm {
    fn acquire_capabilities(&self, capabilities: WidgetCapabilities) -> WidgetCapabilities;
}

struct CapabilitiesProviderWrap(Arc<dyn WidgetCapabilitiesProvider>);

impl matrix_sdk::widget::CapabilitiesProvider for CapabilitiesProviderWrap {
    async fn acquire_capabilities(
        &self,
        capabilities: matrix_sdk::widget::Capabilities,
    ) -> matrix_sdk::widget::Capabilities {
        let this = self.0.clone();
        // This could require a prompt to the user. Ideally the callback
        // interface would just be async, but that's not supported yet so use
        // one of tokio's blocking task threads instead.
        get_runtime_handle()
            .spawn_blocking(move || this.acquire_capabilities(capabilities.into()).into())
            .await
            // propagate panics from the blocking task
            .unwrap()
    }
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum ParseError {
    #[error("empty host")]
    EmptyHost,
    #[error("invalid international domain name")]
    IdnaError,
    #[error("invalid port number")]
    InvalidPort,
    #[error("invalid IPv4 address")]
    InvalidIpv4Address,
    #[error("invalid IPv6 address")]
    InvalidIpv6Address,
    #[error("invalid domain character")]
    InvalidDomainCharacter,
    #[error("relative URL without a base")]
    RelativeUrlWithoutBase,
    #[error("relative URL with a cannot-be-a-base base")]
    RelativeUrlWithCannotBeABaseBase,
    #[error("a cannot-be-a-base URL doesn’t have a host to set")]
    SetHostOnCannotBeABaseUrl,
    #[error("URLs more than 4 GB are not supported")]
    Overflow,
    #[error("unknown URL parsing error")]
    Other,
}

impl From<url::ParseError> for ParseError {
    fn from(value: url::ParseError) -> Self {
        match value {
            url::ParseError::EmptyHost => Self::EmptyHost,
            url::ParseError::IdnaError => Self::IdnaError,
            url::ParseError::InvalidPort => Self::InvalidPort,
            url::ParseError::InvalidIpv4Address => Self::InvalidIpv4Address,
            url::ParseError::InvalidIpv6Address => Self::InvalidIpv6Address,
            url::ParseError::InvalidDomainCharacter => Self::InvalidDomainCharacter,
            url::ParseError::RelativeUrlWithoutBase => Self::RelativeUrlWithoutBase,
            url::ParseError::RelativeUrlWithCannotBeABaseBase => {
                Self::RelativeUrlWithCannotBeABaseBase
            }
            url::ParseError::SetHostOnCannotBeABaseUrl => Self::SetHostOnCannotBeABaseUrl,
            url::ParseError::Overflow => Self::Overflow,
            _ => Self::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use matrix_sdk::widget::Capabilities;

    use super::get_element_call_required_permissions;

    #[test]
    fn element_call_permissions_are_correct() {
        let widget_cap = get_element_call_required_permissions(
            "@my_user:my_domain.org".to_owned(),
            "ABCDEFGHI".to_owned(),
        );

        // We test two things:

        // Converting the WidgetCapability (ffi struct) to Capabilities (rust sdk
        // struct)
        let cap = Into::<Capabilities>::into(widget_cap);
        // Converting Capabilities (rust sdk struct) to a json list.
        let cap_json_repr = serde_json::to_string(&cap).unwrap();

        // Converting to a Vec<String> allows to check if the required elements exist
        // without breaking the test each time the order of permissions might
        // change.
        let permission_array: Vec<String> = serde_json::from_str(&cap_json_repr).unwrap();

        let cap_assert = |capability: &str| {
            assert!(
                permission_array.contains(&capability.to_owned()),
                "The \"{capability}\" capability was missing from the element call capability list."
            );
        };

        cap_assert("io.element.requires_client");
        cap_assert("org.matrix.msc4157.update_delayed_event");
        cap_assert("org.matrix.msc4157.send.delayed_event");
        cap_assert("org.matrix.msc2762.receive.state_event:org.matrix.msc3401.call.member");
        cap_assert("org.matrix.msc2762.receive.state_event:m.room.member");
        cap_assert("org.matrix.msc2762.receive.state_event:m.room.encryption");
        cap_assert("org.matrix.msc2762.receive.event:org.matrix.rageshake_request");
        cap_assert("org.matrix.msc2762.receive.event:io.element.call.encryption_keys");
        cap_assert("org.matrix.msc2762.receive.state_event:m.room.create");
        cap_assert("org.matrix.msc2762.send.state_event:org.matrix.msc3401.call.member#@my_user:my_domain.org");
        cap_assert("org.matrix.msc2762.send.state_event:org.matrix.msc3401.call.member#@my_user:my_domain.org_ABCDEFGHI");
        cap_assert("org.matrix.msc2762.send.state_event:org.matrix.msc3401.call.member#_@my_user:my_domain.org_ABCDEFGHI");
        cap_assert("org.matrix.msc2762.send.event:org.matrix.rageshake_request");
        cap_assert("org.matrix.msc2762.send.event:io.element.call.encryption_keys");
    }
}
