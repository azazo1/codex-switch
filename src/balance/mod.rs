mod providers;

pub(crate) use providers::{
    API_KEY_CREDENTIAL, NEWAPI_USER_ID_CREDENTIAL, NEWAPI_USER_KEY_CREDENTIAL, detect_provider,
    query_and_store,
};
