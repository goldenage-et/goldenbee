//! Stable argument parsing and prompt expansion for Worker command assets.
//!
//! Parsing happens before a command can submit a task, so malformed input never
//! reaches execution. Expansion is a single non-recursive substitution pass:
//! caller text cannot introduce a placeholder or a host command.

use super::asset::CommandAsset;
use common::{ApiError, CommandArgument};
use std::collections::BTreeMap;

/// One validated command invocation.
#[derive(Debug, Clone)]
pub(crate) struct PreparedCommand {
    /// Expanded prompt used as the command agent instruction.
    pub prompt: String,
    /// Resolved argument values.
    pub arguments: BTreeMap<String, String>,
}

/// Split caller input into shell-like tokens without invoking a shell.
///
/// Whitespace separates tokens; single and double quotes group; a backslash
/// escapes the next character outside single quotes. Unbalanced quotes and a
/// trailing escape are stable validation errors.
pub(crate) fn tokenize(input: &str) -> Result<Vec<String>, ApiError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut characters = input.chars();
    while let Some(character) = characters.next() {
        match quote {
            Some(active) => {
                if character == active {
                    quote = None;
                } else if character == '\\' && active == '"' {
                    let Some(escaped) = characters.next() else {
                        return Err(invalid("input ends with an incomplete escape"));
                    };
                    current.push(escaped);
                } else {
                    current.push(character);
                }
            }
            None => match character {
                '\'' | '"' => {
                    quote = Some(character);
                    started = true;
                }
                '\\' => {
                    let Some(escaped) = characters.next() else {
                        return Err(invalid("input ends with an incomplete escape"));
                    };
                    current.push(escaped);
                    started = true;
                }
                character if character.is_whitespace() => {
                    if started {
                        tokens.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                character => {
                    current.push(character);
                    started = true;
                }
            },
        }
    }
    if quote.is_some() {
        return Err(invalid("input has an unterminated quote"));
    }
    if started {
        tokens.push(current);
    }
    Ok(tokens)
}

/// Validate and expand one command with caller input.
pub(crate) fn prepare(asset: &CommandAsset, input: &str) -> Result<PreparedCommand, ApiError> {
    let tokens = tokenize(input)?;
    let (values, normalized) = if asset.arguments.is_empty() {
        let normalized = tokens.join(" ");
        let mut values = BTreeMap::new();
        values.insert("args".to_string(), normalized.clone());
        (values, normalized)
    } else {
        let values = named(&tokens, &asset.arguments)?;
        let normalized = asset
            .arguments
            .iter()
            .filter_map(|argument| values.get(&argument.name).cloned())
            .collect::<Vec<_>>()
            .join(" ");
        (values, normalized)
    };
    let rendered = render(&asset.prompt, &values)?;
    let has_placeholder = CommandAsset::placeholders(&asset.prompt)
        .map(|placeholders| !placeholders.is_empty())
        .unwrap_or(false);
    let prompt = if !has_placeholder && !normalized.is_empty() {
        format!("{rendered}\n\n{normalized}")
    } else {
        rendered
    };
    Ok(PreparedCommand {
        prompt,
        arguments: values,
    })
}

fn named(
    tokens: &[String],
    declarations: &[CommandArgument],
) -> Result<BTreeMap<String, String>, ApiError> {
    let mut values = BTreeMap::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        let Some(rest) = token.strip_prefix("--") else {
            return Err(ApiError::new(
                "command.arguments_unexpected",
                format!("unexpected positional argument '{token}'"),
            ));
        };
        if rest.is_empty() {
            return Err(invalid("'--' is not a command argument"));
        }
        let (name, inline) = match rest.split_once('=') {
            Some((name, value)) => (name, Some(value.to_string())),
            None => (rest, None),
        };
        let declaration = declarations
            .iter()
            .find(|declaration| declaration.name == name)
            .ok_or_else(|| {
                ApiError::new(
                    "command.argument_unknown",
                    format!("unknown command argument '--{name}'"),
                )
            })?;
        if values.contains_key(&declaration.name) {
            return Err(ApiError::new(
                "command.argument_duplicate",
                format!("command argument '--{name}' was supplied more than once"),
            ));
        }
        let value = match inline {
            Some(value) => value,
            None => {
                index += 1;
                let Some(value) = tokens.get(index) else {
                    return Err(ApiError::new(
                        "command.arguments_invalid",
                        format!("command argument '--{name}' requires a value"),
                    ));
                };
                if value.starts_with("--") {
                    return Err(ApiError::new(
                        "command.arguments_invalid",
                        format!("command argument '--{name}' requires a value"),
                    ));
                }
                value.clone()
            }
        };
        values.insert(declaration.name.clone(), value);
        index += 1;
    }
    for declaration in declarations {
        if values.contains_key(&declaration.name) {
            continue;
        }
        if let Some(default) = &declaration.default {
            values.insert(declaration.name.clone(), default.clone());
        } else if declaration.required {
            return Err(ApiError::new(
                "command.arguments_missing",
                format!("command argument '--{}' is required", declaration.name),
            ));
        }
    }
    Ok(values)
}

fn render(prompt: &str, values: &BTreeMap<String, String>) -> Result<String, ApiError> {
    let mut output = String::with_capacity(prompt.len());
    let mut rest = prompt;
    while let Some(start) = rest.find("{{") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or_else(|| {
            ApiError::new(
                "command.placeholder_unresolved",
                "the command prompt has an unterminated placeholder",
            )
        })?;
        let name = after[..end].trim();
        let value = values.get(name).ok_or_else(|| {
            ApiError::new(
                "command.placeholder_unresolved",
                format!("the command prompt needs argument '{{{name}}}'"),
            )
        })?;
        output.push_str(value);
        rest = &after[end + 2..];
    }
    output.push_str(rest);
    Ok(output)
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new("command.arguments_invalid", message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(prompt: &str, arguments: Vec<CommandArgument>) -> CommandAsset {
        CommandAsset {
            name: "test".into(),
            description: String::new(),
            prompt: prompt.into(),
            allowed_tools: Vec::new(),
            model: None,
            agent: None,
            arguments,
        }
    }

    #[test]
    fn free_form_arguments_preserve_quoted_text() {
        let prepared = prepare(
            &asset("Say {{args}}", Vec::new()),
            r#"one "two words" 'three words'"#,
        )
        .expect("prepared");
        assert_eq!(prepared.prompt, "Say one two words three words");
        assert_eq!(prepared.arguments["args"], "one two words three words");
    }

    #[test]
    fn free_form_prompt_without_placeholder_appends_input() {
        let prepared = prepare(&asset("Do the task.", Vec::new()), "payload").expect("prepared");
        assert_eq!(prepared.prompt, "Do the task.\n\npayload");
    }

    #[test]
    fn declared_arguments_support_defaults_and_required_values() {
        let declaration = vec![
            CommandArgument {
                name: "topic".into(),
                description: None,
                required: true,
                default: None,
            },
            CommandArgument {
                name: "style".into(),
                description: None,
                required: false,
                default: Some("brief".into()),
            },
        ];
        let prepared = prepare(
            &asset("{{topic}} in {{style}} style", declaration.clone()),
            "--topic releases",
        )
        .expect("prepared");
        assert_eq!(prepared.prompt, "releases in brief style");
        assert_eq!(prepared.arguments["style"], "brief");

        let missing = prepare(&asset("{{topic}}", declaration), "").expect_err("required");
        assert_eq!(missing.code, "command.arguments_missing");
    }

    #[test]
    fn unknown_duplicate_and_positional_arguments_are_stable_errors() {
        let declaration = vec![CommandArgument {
            name: "topic".into(),
            description: None,
            required: true,
            default: None,
        }];
        let unknown = prepare(&asset("{{topic}}", declaration.clone()), "--other x")
            .expect_err("unknown argument");
        assert_eq!(unknown.code, "command.argument_unknown");
        let duplicate = prepare(
            &asset("{{topic}}", declaration.clone()),
            "--topic a --topic b",
        )
        .expect_err("duplicate");
        assert_eq!(duplicate.code, "command.argument_duplicate");
        let positional = prepare(&asset("{{topic}}", declaration), "bare").expect_err("positional");
        assert_eq!(positional.code, "command.arguments_unexpected");
    }

    #[test]
    fn malformed_quoting_never_produces_a_prompt() {
        let error =
            prepare(&asset("{{args}}", Vec::new()), "\"unterminated").expect_err("malformed quote");
        assert_eq!(error.code, "command.arguments_invalid");
    }
}
