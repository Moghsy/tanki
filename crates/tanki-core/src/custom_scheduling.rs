//! Desktop-compatible state transforms in a bounded, isolated QuickJS runtime.
use anki_proto::scheduler::{SchedulingContext, SchedulingStates};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, SerializeOptions};
use rquickjs::{Context, Promise, Runtime};
use std::time::{Duration, Instant};

pub(crate) fn apply(
    states: &SchedulingStates,
    context: &SchedulingContext,
    script: &str,
) -> std::result::Result<SchedulingStates, String> {
    let run = || -> std::result::Result<SchedulingStates, Box<dyn std::error::Error>> {
        let pool = DescriptorPool::decode(
            include_bytes!(concat!(env!("OUT_DIR"), "/scheduler.bin")).as_slice(),
        )?;
        let descriptor = pool
            .get_message_by_name("anki.scheduler.SchedulingStates")
            .unwrap();
        let dynamic =
            DynamicMessage::decode(descriptor.clone(), states.encode_to_vec().as_slice())?;
        let mut bytes = Vec::new();
        dynamic.serialize_with_options(
            &mut serde_json::Serializer::new(&mut bytes),
            &SerializeOptions::new().skip_default_fields(false),
        )?;
        // SchedulingContext is a JS protobuf object on desktop: seed is a bigint.
        let mut context_json = serde_json::json!({"deckName":context.deck_name});
        if let Some(decay) = context.decay {
            context_json["decay"] = serde_json::json!(decay);
        }
        if let Some(retention) = context.desired_retention {
            context_json["desiredRetention"] = serde_json::json!(retention);
        }
        let source = format!(
            r#"(async () => {{
            globalThis.console = {{ log() {{}}, warn() {{}}, error() {{}} }};
            const states = {states};
            const ctx = {context}; ctx.seed = BigInt("{seed}");
            const customData = {{}};
            for (const rating of ['again','hard','good','easy']) {{
                try {{ customData[rating] = JSON.parse(states.current.customData); }}
                catch (_) {{ customData[rating] = {{}}; }}
            }}
            await (async (states, customData, ctx) => {{ {script}
 }})(states, customData, ctx);
            for (const rating of ['again','hard','good','easy']) states[rating].customData = JSON.stringify(customData[rating]);
            return JSON.stringify(states);
        }})()"#,
            states = String::from_utf8(bytes)?,
            context = context_json,
            seed = context.seed
        );
        let rt = Runtime::new()?;
        rt.set_memory_limit(16 * 1024 * 1024);
        rt.set_max_stack_size(512 * 1024);
        let deadline = Instant::now() + Duration::from_secs(1);
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
        let ctx = Context::full(&rt)?;
        let json = ctx
            .with(|ctx| -> std::result::Result<String, String> {
                let result = (|| {
                    let promise: Promise = ctx.eval(source)?;
                    promise.finish::<String>()
                })();
                result.map_err(|error| format!("{error}: {:?}", ctx.catch()))
            })
            .map_err(std::io::Error::other)?;
        let dynamic = DynamicMessage::deserialize(
            descriptor,
            &mut serde_json::Deserializer::from_str(&json),
        )?;
        Ok(SchedulingStates::decode(
            dynamic.encode_to_vec().as_slice(),
        )?)
    };
    run().map_err(|e| format!("Custom scheduling failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anki_proto::scheduler::{scheduling_state as s, SchedulingState};
    fn states() -> SchedulingStates {
        let state = SchedulingState {
            kind: Some(s::Kind::Normal(s::Normal {
                kind: Some(s::normal::Kind::Review(s::Review {
                    scheduled_days: 100,
                    ..Default::default()
                })),
            })),
            custom_data: Some("invalid json".into()),
        };
        SchedulingStates {
            current: Some(state.clone()),
            again: Some(state.clone()),
            hard: Some(state.clone()),
            good: Some(state.clone()),
            easy: Some(state),
        }
    }
    #[test]
    fn protobuf_shape_async_context_and_independent_custom_data() {
        let original = states();
        let context = SchedulingContext {
            deck_name: "Study::French".into(),
            seed: u64::MAX,
            ..Default::default()
        };
        let updated = apply(&original, &context, r#"
            await Promise.resolve();
            if (states.good.normal.review.elapsedDays !== 0 || states.good.normal.review.leeched !== false) throw Error('defaults');
            if ('memoryState' in states.good.normal.review || 'filtered' in states.good) throw Error('presence');
            if (ctx.seed !== 18446744073709551615n) throw Error('seed');
            states.good.normal.review.scheduledDays = 7;
            customData.good.d = ctx.deckName;
            console.log('ignored');
        "#).unwrap();
        let good = updated.good.unwrap();
        assert_eq!(
            good.custom_data.as_deref(),
            Some(r#"{"d":"Study::French"}"#)
        );
        assert_eq!(updated.hard.unwrap().custom_data.as_deref(), Some("{}"));
        let Some(s::Kind::Normal(s::Normal {
            kind: Some(s::normal::Kind::Review(review)),
        })) = good.kind
        else {
            panic!()
        };
        assert_eq!(review.scheduled_days, 7);
        assert_eq!(original, states());
    }
    #[test]
    fn failures_are_bounded_and_never_modify_input() {
        for script in [
            "throw Error('broken')",
            "await Promise.reject('broken')",
            "not valid {",
            "while (true) {}",
            "await new Promise(() => {})",
            "states.good.normal.review.scheduledDays = -1",
            "let x=[]; while(true) x.push('x'.repeat(100000))",
        ] {
            let original = states();
            let start = Instant::now();
            assert!(
                apply(&original, &SchedulingContext::default(), script).is_err(),
                "{script}"
            );
            assert!(start.elapsed() < Duration::from_secs(3));
            assert_eq!(original, states());
        }
    }
}
