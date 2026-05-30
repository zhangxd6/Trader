//! Safety enforcement layer that sits between the LLM and the broker.

mod validator;

pub use validator::SafetyValidator;
