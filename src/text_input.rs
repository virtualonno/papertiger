use anyhow::{Context, Result, bail};
use clap::Args;
use std::io::Read;

fn read_text(
    field: &str,
    inline: Option<String>,
    file: Option<String>,
    allow_blank: bool,
) -> Result<Option<String>> {
    let (value, external) = match (inline, file) {
        (Some(_), Some(_)) => {
            bail!("pass --{field} or --{field}-file, not both");
        }
        (Some(value), None) => (value, false),
        (None, Some(path)) if path == "-" => {
            let mut value = String::new();
            std::io::stdin()
                .read_to_string(&mut value)
                .with_context(|| format!("read --{field}-file - from stdin"))?;
            (value, true)
        }
        (None, Some(path)) => (
            std::fs::read_to_string(&path)
                .with_context(|| format!("read --{field}-file {path}"))?,
            true,
        ),
        (None, None) => return Ok(None),
    };
    let value = if external {
        value.strip_prefix('\u{feff}').unwrap_or(&value)
    } else {
        &value
    };
    let value = value.trim();
    if value.is_empty() && !allow_blank {
        bail!("--{field} requires nonblank text; pass --{field} or --{field}-file with content");
    }
    Ok(Some(value.to_owned()))
}

macro_rules! text_args {
    (
        $name:ident,
        $field:ident,
        $file:ident,
        $label:literal,
        $allow_blank:literal,
        $inline_help:literal,
        $file_help:literal,
        optional
    ) => {
        #[derive(Debug, Args)]
        pub struct $name {
            #[arg(long, conflicts_with = stringify!($file), help = $inline_help)]
            pub $field: Option<String>,
            #[arg(long, value_name = "PATH|-", help = $file_help)]
            pub $file: Option<String>,
        }

        impl $name {
            pub fn optional(self) -> Result<Option<String>> {
                read_text($label, self.$field, self.$file, $allow_blank)
            }
        }
    };
    (
        $name:ident,
        $field:ident,
        $file:ident,
        $label:literal,
        $allow_blank:literal,
        $inline_help:literal,
        $file_help:literal,
        required
    ) => {
        text_args!(
            $name,
            $field,
            $file,
            $label,
            $allow_blank,
            $inline_help,
            $file_help,
            optional
        );

        impl $name {
            pub fn required(self) -> Result<String> {
                self.optional()?.ok_or_else(|| {
                    anyhow::anyhow!("pass --{} or --{}-file with nonblank text", $label, $label)
                })
            }
        }
    };
}

text_args!(
    IntentArgs,
    intent,
    intent_file,
    "intent",
    true,
    "Durable orientation text; blank clears it when editing",
    "Read durable orientation as UTF-8 from PATH, or stdin with '-'",
    optional
);
text_args!(
    ResultArgs,
    result,
    result_file,
    "result",
    false,
    "Durable measured or selected outcome",
    "Read the durable outcome as UTF-8 from PATH, or stdin with '-'",
    optional
);
text_args!(
    WhyArgs,
    why,
    why_file,
    "why",
    false,
    "Standalone rationale for this mutation",
    "Read the rationale as UTF-8 from PATH, or stdin with '-'",
    required
);

impl IntentArgs {
    pub fn reads_stdin(&self) -> bool {
        self.intent_file.as_deref() == Some("-")
    }
}

impl WhyArgs {
    pub fn reads_stdin(&self) -> bool {
        self.why_file.as_deref() == Some("-")
    }
}

pub fn reject_multiple_stdin(fields: &[(&str, bool)]) -> Result<()> {
    let stdin_fields = fields
        .iter()
        .filter_map(|(field, reads_stdin)| reads_stdin.then_some(*field))
        .collect::<Vec<_>>();
    if stdin_fields.len() > 1 {
        bail!(
            "stdin can supply only one text field per command; use a file path for one of {}",
            stdin_fields
                .iter()
                .map(|field| format!("--{field}-file"))
                .collect::<Vec<_>>()
                .join(" or ")
        );
    }
    Ok(())
}

#[derive(Debug, Args)]
pub struct NoteTextArgs {
    #[arg(
        value_name = "TEXT",
        conflicts_with = "text_file",
        help = "Durable note text"
    )]
    pub text: Option<String>,
    #[arg(
        long,
        value_name = "PATH|-",
        help = "Read the note as UTF-8 from PATH, or stdin with '-'"
    )]
    pub text_file: Option<String>,
}

impl NoteTextArgs {
    pub fn required(self) -> Result<String> {
        read_text("text", self.text, self.text_file, false)?.ok_or_else(|| {
            anyhow::anyhow!("pass note TEXT or note --text-file <path|-> with nonblank text")
        })
    }
}
