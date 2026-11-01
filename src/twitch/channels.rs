use std::{
    string::{String, ToString},
    vec::Vec,
};

use color_eyre::Result;

use super::api::following::{Following, get_following};
use crate::{ui::components::utils::SearchItemGetter, utils::sanitization::clean_channel_name};

impl SearchItemGetter<String> for Following {
    async fn get_items(&mut self) -> Result<Vec<String>> {
        let following = get_following(
            self.twitch_oauth.clone(),
            self.config.frontend.only_get_live_followed_channels,
        )
        .await;

        let unfollowed = self.storage.borrow().unfollowed_channels();
        let manual_followed = self.storage.borrow().followed_channels();

        following.map(|v| {
            let mut list: Vec<String> = v
                .data
                .iter()
                .map(ToString::to_string)
                .filter(|item| {
                    let login = clean_channel_name(item.split(':').next().unwrap_or(item));
                    !unfollowed.iter().any(|u| u.eq_ignore_ascii_case(&login))
                })
                .collect();

            for mf in manual_followed {
                let clean = clean_channel_name(&mf);
                if !clean.is_empty()
                    && !list.iter().any(|item| {
                        let login = clean_channel_name(item.split(':').next().unwrap_or(item));
                        login.eq_ignore_ascii_case(&clean)
                    })
                {
                    list.push(clean);
                }
            }

            list
        })
    }

    fn remove_item(&mut self, item: &str) {
        let login = clean_channel_name(item.split(':').next().unwrap_or(item));
        if !login.is_empty() {
            self.storage.borrow_mut().unfollow_channel(&login);
        }
    }
}
