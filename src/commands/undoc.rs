use crate::service::iot::start_iot_client;
use std::sync::Arc;

#[derive(clap::Parser, Debug)]
pub struct UndocCommand {
    #[command(subcommand)]
    cmd: SubCommand,
}

#[derive(clap::Parser, Debug)]
#[allow(clippy::enum_variant_names)]
enum SubCommand {
    DumpOneClick {},
    ShowOneClick {},
    OneClick {
        name: String,
    },
    /// Test-only login probe. Emits only a redacted, machine-readable outcome.
    ///
    /// The name is pinned rather than derived. scripts/live_2fa.py invokes it
    /// as a literal string, so the two are a cross-language contract; leaving
    /// it to heck's word-splitting would make that contract depend on a
    /// transitive dependency's casing rules. Asserted by the test below.
    #[cfg(feature = "live-2fa-test")]
    #[command(name = "live2fa-login")]
    Live2faLogin {},
}

impl UndocCommand {
    pub async fn run(&self, args: &crate::Args) -> anyhow::Result<()> {
        match &self.cmd {
            SubCommand::DumpOneClick {} => {
                let client = args.undoc_args.api_client()?;
                let token = client.login_community().await?;
                let res = client.get_saved_one_click_shortcuts(&token).await?;

                println!("{res:#?}");
            }
            SubCommand::ShowOneClick {} => {
                let client = args.undoc_args.api_client()?;
                let items = client.parse_one_clicks().await?;
                println!("{items:#?}");
            }
            SubCommand::OneClick { name } => {
                let client = args.undoc_args.api_client()?;
                let items = client.parse_one_clicks().await?;
                let item = items
                    .iter()
                    .find(|item| &item.name == name)
                    .ok_or_else(|| anyhow::anyhow!("didn't find item {name}"))?;

                let state = Arc::new(crate::service::state::State::new());
                start_iot_client(&args.undoc_args, state.clone(), None).await?;
                let iot = state.get_iot_client().await.expect("just started iot");

                iot.activate_one_click(item).await?;
            }
            #[cfg(feature = "live-2fa-test")]
            SubCommand::Live2faLogin {} => {
                let client = args.undoc_args.api_client()?;
                match client.login_account().await {
                    Ok(_) => println!(r#"{{"schema":1,"outcome":"login_ok"}}"#),
                    Err(err) => {
                        if let Some((status, code_configured)) =
                            crate::undoc_api::classify_2fa_login_error(&err)
                        {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "schema": 1,
                                    "outcome": "two_factor",
                                    "status": status,
                                    "code_configured": code_configured,
                                })
                            );
                        } else {
                            println!(r#"{{"schema":1,"outcome":"unexpected_error"}}"#);
                            anyhow::bail!(
                                "live 2FA probe failed; upstream details were intentionally redacted"
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "live-2fa-test"))]
mod test {
    use clap::CommandFactory;

    /// The probe's subcommand name is a contract between two languages:
    /// `scripts/live_2fa.py` spawns `undoc live2fa-login` as a literal string.
    /// Read the harness source rather than restating the name, so the two can
    /// never drift apart silently -- a rename on either side fails here.
    #[test]
    fn probe_subcommand_name_matches_the_harness_invocation() {
        let rendered: Vec<String> = super::UndocCommand::command()
            .get_subcommands()
            .map(|sub| sub.get_name().to_string())
            .collect();

        let harness =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/live_2fa.py"))
                .expect("read the live 2FA harness");
        let invoked = harness
            .split_once(r#""undoc", ""#)
            .map(|(_, rest)| rest.split('"').next().expect("closing quote"))
            .expect("harness invokes a subcommand of `undoc`");

        assert!(
            rendered.iter().any(|name| name == invoked),
            "harness runs `undoc {invoked}` but clap renders {rendered:?}"
        );
    }
}
