use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::value::Value;
use std::io::{Read, Write};
use uuid::Uuid;

use crate::{
    ClientTransport, MethodId, PartialMethodId, RPCError, RPCErrorKind, Result, ServerTransport,
};

pub struct JTXState {
    method: &'static str,
    params: Value,
}

pub struct JRXState {
    json: Value,
}

/// Transport implementation over JSON-RPC. Can be used over any
/// `Read+Write` channel (local socket, internet socket, pipe,
/// etc). Enable the "json_transport" feature to use this.
pub struct JSONTransport<C: Read + Write> {
    channel: C,
}

impl<C: Read + Write> JSONTransport<C> {
    pub fn new(channel: C) -> Self {
        JSONTransport { channel }
    }

    /// Get the underlying read/write channel
    pub fn channel(&self) -> &C {
        &self.channel
    }

    // Deserialize a value from the channel
    fn read_from_channel<T>(&mut self) -> Result<T>
    where
        for<'de> T: serde::Deserialize<'de>,
    {
        read_value_from_json(Read::by_ref(&mut self.channel))
    }

    fn flush(&mut self) -> Result<()> {
        self.channel.flush().map_err(|e| {
            RPCError::with_cause(
                RPCErrorKind::SerializationError,
                "cannot flush underlying channel",
                e,
            )
        })
    }
}
impl<C: Read + Write> ClientTransport for JSONTransport<C> {
    type TXState = JTXState;
    type FinalState = ();

    fn tx_begin_call(&mut self, method: MethodId) -> Result<JTXState> {
        Ok(begin_call(method))
    }

    fn tx_add_param(
        &mut self,
        name: &'static str,
        value: impl Serialize,
        state: &mut JTXState,
    ) -> Result<()> {
        add_param(name, value, state)
    }

    fn tx_finalize(&mut self, state: JTXState) -> Result<()> {
        serde_json::to_writer(Write::by_ref(&mut self.channel), &value_for_state(&state))
            .map_err(convert_error)?;
        self.flush()
    }

    fn rx_response<T>(&mut self, _state: ()) -> Result<T>
    where
        for<'de> T: Deserialize<'de>,
    {
        self.read_from_channel()
    }
}

fn convert_error(e: impl std::error::Error) -> RPCError {
    RPCError::with_cause(
        RPCErrorKind::SerializationError,
        "json serialization or deserialization failed",
        e,
    )
}

fn begin_call(method: MethodId) -> JTXState {
    JTXState {
        method: method.name,
        params: json!({}),
    }
}

fn value_for_state(state: &JTXState) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "method": state.method,
        "params": state.params,
        "id": format!("{}", Uuid::new_v4())
    })
}

fn add_param(name: &'static str, value: impl Serialize, state: &mut JTXState) -> Result<()> {
    state.params.as_object_mut().unwrap().insert(
        name.to_string(),
        serde_json::to_value(value).map_err(convert_error)?,
    );
    Ok(())
}

fn read_value_from_json<T, R>(reader: R) -> Result<T>
where
    for<'de> T: serde::Deserialize<'de>,
    R: Read,
{
    let read = serde_json::de::IoRead::new(reader);
    let mut de = serde_json::de::Deserializer::new(read);
    serde::de::Deserialize::deserialize(&mut de).map_err(|e| {
        println!("classification {:?}", e.classify());
        if e.classify() == serde_json::error::Category::Eof {
            RPCError::new(
                RPCErrorKind::TransportEOF,
                "EOF during json deserialization",
            )
        } else {
            convert_error(e)
        }
    })
}

impl<C: Read + Write> ServerTransport for JSONTransport<C> {
    type RXState = JRXState;

    fn rx_begin_call(&mut self) -> Result<(PartialMethodId, JRXState)> {
        let value: Value = self.read_from_channel()?;
        let method = value
            .get("method")
            .ok_or_else(|| {
                RPCError::new(
                    RPCErrorKind::SerializationError,
                    "json is not expected object",
                )
            })?
            .as_str()
            .ok_or_else(|| {
                RPCError::new(
                    RPCErrorKind::SerializationError,
                    "json method was not string",
                )
            })?
            .to_string();
        Ok((PartialMethodId::Name(method), JRXState { json: value }))
    }

    fn rx_read_param<T>(&mut self, name: &'static str, state: &mut JRXState) -> Result<T>
    where
        for<'de> T: serde::Deserialize<'de>,
    {
        let param_val = state
            .json
            .get("params")
            .ok_or_else(|| {
                RPCError::new(
                    RPCErrorKind::SerializationError,
                    "json is not expected object",
                )
            })?
            .get(name)
            .ok_or_else(|| {
                RPCError::new(
                    RPCErrorKind::SerializationError,
                    format!("parameters do not contain {}", name),
                )
            })?;
        serde_json::from_value(param_val.clone()).map_err(convert_error)
    }

    fn tx_response(&mut self, value: impl Serialize) -> Result<()> {
        let res = serde_json::to_writer(Write::by_ref(&mut self.channel), &value)
            .map_err(convert_error)?;
        self.flush()?;
        Ok(res)
    }
}

#[cfg(feature = "async_client")]
mod async_client {
    use super::*;
    use crate::AsyncClientTransport;
    use futures::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    /// Like JSONTransport except for use as AsyncClientTransport.
    pub struct JSONAsyncClientTransport<C: AsyncRead + AsyncWrite> {
        channel: C,
    }

    impl<C: AsyncRead + AsyncWrite> JSONAsyncClientTransport<C> {
        /// Create an AsyncJSONTransport.
        pub fn new(channel: C) -> Self {
            JSONAsyncClientTransport { channel }
        }
    }

    #[async_trait]
    impl<C: AsyncRead + AsyncWrite + Send + Unpin> AsyncClientTransport
        for JSONAsyncClientTransport<C>
    {
        type TXState = JTXState;
        type FinalState = ();

        async fn tx_begin_call(&mut self, method: MethodId) -> Result<JTXState> {
            Ok(begin_call(method))
        }

        async fn tx_add_param(
            &mut self,
            name: &'static str,
            value: impl Serialize + Send + 'async_trait,
            state: &mut JTXState,
        ) -> Result<()> {
            add_param(name, value, state)
        }

        async fn tx_finalize(&mut self, state: JTXState) -> Result<()> {
            let j = serde_json::to_vec(&value_for_state(&state)).map_err(convert_error)?;
            self.channel.write(&j).await?;
            self.channel.flush().await?;
            Ok(())
        }

        async fn rx_response<T>(&mut self, _state: ()) -> Result<T>
        where
            for<'de> T: Deserialize<'de>,
        {
            println!("rx response");
            // TODO address limitations
            let mut data = [0u8; 1024];
            self.channel.read(&mut data).await?;
            read_value_from_json(&data as &[u8])
        }
    }
}

#[cfg(feature = "async_client")]
pub use self::async_client::JSONAsyncClientTransport;
