//! Nostr event watcher for hashtree merkle root updates.
//!
//! Subscribes to Nostr relays and watches for events from tracked npubs
//! that indicate merkle root updates (new commits).
//!
//! ## Event Format (NIP-78 / Kind 30078)
//!
//! Hashtree publishes tree roots as replaceable parameterized events:
//! - Kind: 30078
//! - `d` tag: tree name (e.g., "hashtree-ts")
//! - `l` tag: "hashtree" (label for filtering)
//! - `hash` tag: merkle root hash (hex)
//! - `key` tag: optional encryption key (for public trees)

use ci_core::{HashtreeConfig, RunnerConfig, WatchedRepo};
use nostr_sdk::prelude::*;
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Event indicating a repository was updated
#[derive(Debug, Clone, PartialEq)]
pub struct RepoUpdate {
    pub owner_npub: String,
    pub path: String,
    pub merkle_root: String,
    pub timestamp: Timestamp,
}

/// Watcher that subscribes to Nostr events for tracked repos
pub struct RepoWatcher {
    client: Client,
    watched_repos: Vec<WatchedRepo>,
    update_tx: mpsc::Sender<RepoUpdate>,
}

/// Parse a hashtree event to extract the merkle root hash
fn parse_hashtree_event(event: &Event) -> Option<(String, String)> {
    // Check for 'l' tag with 'hashtree' label
    let has_hashtree_label = event.tags.iter().any(|t| {
        let tag_vec: Vec<String> = t.clone().to_vec();
        tag_vec.len() >= 2 && tag_vec[0] == "l" && tag_vec[1] == "hashtree"
    });

    if !has_hashtree_label {
        return None;
    }

    // Get 'd' tag (tree name)
    let d_tag = event.tags.iter().find_map(|t| {
        let tag_vec: Vec<String> = t.clone().to_vec();
        if tag_vec.len() >= 2 && tag_vec[0] == "d" {
            Some(tag_vec[1].clone())
        } else {
            None
        }
    })?;

    // Get 'hash' tag (merkle root)
    let hash = event.tags.iter().find_map(|t| {
        let tag_vec: Vec<String> = t.clone().to_vec();
        if tag_vec.len() >= 2 && tag_vec[0] == "hash" {
            Some(tag_vec[1].clone())
        } else {
            None
        }
    })?;

    Some((d_tag, hash))
}

impl RepoWatcher {
    /// Create a new watcher
    pub async fn new(
        runner_config: &RunnerConfig,
        update_tx: mpsc::Sender<RepoUpdate>,
    ) -> anyhow::Result<Self> {
        // Load network config for relays
        let hashtree_config = HashtreeConfig::load().unwrap_or_default();

        // Create Nostr client
        let keys = Keys::parse(&runner_config.runner.nsec)?;
        let client = Client::new(keys);

        // Add relays
        for relay in &hashtree_config.network.relays {
            tracing::info!("Adding relay: {}", relay);
            client.add_relay(relay).await?;
        }

        // Connect to relays
        client.connect().await;

        Ok(Self {
            client,
            watched_repos: runner_config.runner.watched_repos.clone(),
            update_tx,
        })
    }

    /// Create a watcher with custom relays (for testing)
    pub async fn with_relays(
        runner_config: &RunnerConfig,
        relays: Vec<String>,
        update_tx: mpsc::Sender<RepoUpdate>,
    ) -> anyhow::Result<Self> {
        // Create Nostr client
        let keys = Keys::parse(&runner_config.runner.nsec)?;
        let client = Client::new(keys);

        // Add relays
        for relay in &relays {
            tracing::info!("Adding relay: {}", relay);
            client.add_relay(relay).await?;
        }

        // Connect to relays
        client.connect().await;

        Ok(Self {
            client,
            watched_repos: runner_config.runner.watched_repos.clone(),
            update_tx,
        })
    }

    /// Start watching for updates
    pub async fn watch(&self) -> anyhow::Result<()> {
        if self.watched_repos.is_empty() {
            tracing::warn!("No repos configured to watch");
            return Ok(());
        }

        // Build filter for watched npubs
        let mut pubkeys = Vec::new();
        let mut path_map: HashMap<String, Vec<String>> = HashMap::new();

        for repo in &self.watched_repos {
            let npub = repo.owner_npub.clone();
            if let Ok(pk) = PublicKey::from_bech32(&npub) {
                pubkeys.push(pk);
                path_map
                    .entry(pk.to_hex())
                    .or_default()
                    .push(repo.path.clone());
            } else {
                tracing::warn!("Invalid npub: {}", npub);
            }
        }

        tracing::info!(
            "Watching {} repos from {} npubs",
            self.watched_repos.len(),
            pubkeys.len()
        );

        // Subscribe to kind 30078 events with 'hashtree' label
        let filter = Filter::new()
            .authors(pubkeys.clone())
            .kind(Kind::Custom(30078))
            .custom_tag(SingleLetterTag::lowercase(Alphabet::L), vec!["hashtree"])
            .since(Timestamp::now());

        tracing::info!("Subscribing to events...");

        // Subscribe and handle events
        let subscription_output = self.client.subscribe(vec![filter], None).await?;
        tracing::info!("Subscribed with ID: {:?}", subscription_output.val);

        // Handle notifications
        let update_tx = self.update_tx.clone();
        let watched_repos = self.watched_repos.clone();

        self.client
            .handle_notifications(|notification| async {
                if let RelayPoolNotification::Event { event, .. } = notification {
                    tracing::debug!("Received event: {:?}", event.kind);

                    // Parse the hashtree event
                    if let Some((tree_name, merkle_root)) = parse_hashtree_event(&event) {
                        let author_hex = event.pubkey.to_hex();

                        // Check if this tree is being watched
                        for repo in &watched_repos {
                            if let Ok(pk) = PublicKey::from_bech32(&repo.owner_npub) {
                                if pk.to_hex() == author_hex && tree_name == repo.path {
                                    tracing::info!(
                                        "Detected update for {}/{}: {}",
                                        repo.owner_npub,
                                        repo.path,
                                        &merkle_root[..16.min(merkle_root.len())]
                                    );

                                    let update = RepoUpdate {
                                        owner_npub: repo.owner_npub.clone(),
                                        path: repo.path.clone(),
                                        merkle_root,
                                        timestamp: event.created_at,
                                    };

                                    if let Err(e) = update_tx.send(update).await {
                                        tracing::error!("Failed to send update: {}", e);
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }
                Ok(false) // Continue listening
            })
            .await?;

        Ok(())
    }

    /// Disconnect from relays
    pub async fn disconnect(&self) -> anyhow::Result<()> {
        self.client.disconnect().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hashtree_event_valid() {
        // Create a mock event with hashtree tags
        let keys = Keys::generate();
        let tags = vec![
            Tag::parse(&["d", "hashtree-ts"]).unwrap(),
            Tag::parse(&["l", "hashtree"]).unwrap(),
            Tag::parse(&["hash", "abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234"]).unwrap(),
        ];

        let event = EventBuilder::new(Kind::Custom(30078), "", tags)
            .to_event(&keys)
            .unwrap();

        let result = parse_hashtree_event(&event);
        assert!(result.is_some());
        let (tree_name, hash) = result.unwrap();
        assert_eq!(tree_name, "hashtree-ts");
        assert_eq!(
            hash,
            "abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234"
        );
    }

    #[test]
    fn test_parse_hashtree_event_missing_label() {
        let keys = Keys::generate();
        let tags = vec![
            Tag::parse(&["d", "hashtree-ts"]).unwrap(),
            // Missing 'l' tag with 'hashtree'
            Tag::parse(&["hash", "abcd1234"]).unwrap(),
        ];

        let event = EventBuilder::new(Kind::Custom(30078), "", tags)
            .to_event(&keys)
            .unwrap();

        let result = parse_hashtree_event(&event);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_hashtree_event_missing_hash() {
        let keys = Keys::generate();
        let tags = vec![
            Tag::parse(&["d", "hashtree-ts"]).unwrap(),
            Tag::parse(&["l", "hashtree"]).unwrap(),
            // Missing 'hash' tag
        ];

        let event = EventBuilder::new(Kind::Custom(30078), "", tags)
            .to_event(&keys)
            .unwrap();

        let result = parse_hashtree_event(&event);
        assert!(result.is_none());
    }
}
