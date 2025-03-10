use {
    crate::{
        error::MetaplexError,
        processor::redeem_printing_v2_bid::{create_or_update_prize_tracking, mint_edition},
        state::{
            AuctionManager, NonWinningConstraint, ParticipationConfigV2, Store, WinningConstraint,
            PREFIX,
        },
        utils::{
            assert_derivation, assert_initialized, assert_is_ata, assert_owned_by,
            common_redeem_checks, common_redeem_finish, get_amount_from_token_account,
            spl_token_transfer, CommonRedeemCheckArgs, CommonRedeemFinishArgs, CommonRedeemReturn,
        },
    },
    mpl_auction::processor::{AuctionData, AuctionDataExtended, BidderMetadata},
    mpl_token_metadata::utils::get_supply_off_master_edition,
    solana_program::{
        account_info::{next_account_info, AccountInfo},
        entrypoint::ProgramResult,
        pubkey::Pubkey,
    },
    spl_token::state::Account,
};

struct LegacyAccounts<'a, 'b> {
    pub participation_printing_holding_account_info: &'a AccountInfo<'b>,
}

struct V2Accounts<'a, 'b> {
    pub prize_tracking_ticket_info: &'a AccountInfo<'b>,
    pub new_metadata_account_info: &'a AccountInfo<'b>,
    pub new_edition_account_info: &'a AccountInfo<'b>,
    pub master_edition_account_info: &'a AccountInfo<'b>,
    pub mint_info: &'a AccountInfo<'b>,
    pub edition_marker_info: &'a AccountInfo<'b>,
    pub mint_authority_info: &'a AccountInfo<'b>,
    pub metadata_account_info: &'a AccountInfo<'b>,
    pub auction_extended_info: &'a AccountInfo<'b>,
}

fn legacy_validation(
    token_program_info: &AccountInfo,
    auction_manager: &Box<dyn AuctionManager>,
    accounts: &LegacyAccounts,
) -> ProgramResult {
    assert_owned_by(
        accounts.participation_printing_holding_account_info,
        token_program_info.key,
    )?;

    let participation_printing_account: Account =
        assert_initialized(accounts.participation_printing_holding_account_info)?;

    if participation_printing_account.amount == 0 {
        return Err(MetaplexError::ParticipationPrintingEmpty.into());
    }

    auction_manager
        .assert_legacy_printing_token_match(accounts.participation_printing_holding_account_info)?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn v2_validation<'a, 'b>(
    program_id: &Pubkey,
    auction_manager_info: &AccountInfo<'b>,
    store_info: &AccountInfo<'b>,
    vault_info: &AccountInfo<'b>,
    payer_info: &AccountInfo<'b>,
    token_program_info: &AccountInfo<'b>,
    system_info: &AccountInfo<'b>,
    rent_info: &AccountInfo<'b>,
    bidder_info: &AccountInfo<'b>,
    master_edition_account_info: &AccountInfo<'b>,
    destination_info: &AccountInfo<'b>,
    auction_info: &AccountInfo<'b>,
    config: &ParticipationConfigV2,
    accounts: &V2Accounts<'a, 'b>,
) -> ProgramResult {
    let extended = AuctionDataExtended::from_account_info(accounts.auction_extended_info)?;
    let store = Store::from_account_info(store_info)?;
    let destination_amount = get_amount_from_token_account(destination_info)?;
    assert_is_ata(
        destination_info,
        bidder_info.key,
        token_program_info.key,
        accounts.mint_info.key,
    )?;

    if destination_amount != 1 {
        return Err(MetaplexError::ProvidedAccountDoesNotContainOneToken.into());
    }

    assert_derivation(
        &store.auction_program,
        accounts.auction_extended_info,
        &[
            mpl_auction::PREFIX.as_bytes(),
            store.auction_program.as_ref(),
            vault_info.key.as_ref(),
            mpl_auction::EXTENDED.as_bytes(),
        ],
    )?;

    let mut amount_to_mint = extended.total_uncancelled_bids;
    let num_winners = AuctionData::get_num_winners(auction_info) as u64;
    if config.winner_constraint == WinningConstraint::NoParticipationPrize {
        amount_to_mint = amount_to_mint
            .checked_sub(num_winners)
            .ok_or(MetaplexError::NumericalOverflowError)?;
    } else if config.non_winning_constraint == NonWinningConstraint::NoParticipationPrize {
        amount_to_mint = num_winners
    }

    create_or_update_prize_tracking(
        program_id,
        auction_manager_info,
        accounts.prize_tracking_ticket_info,
        accounts.metadata_account_info,
        payer_info,
        rent_info,
        system_info,
        master_edition_account_info,
        amount_to_mint,
    )?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn v2_transfer<'a, 'b>(
    auction_manager_info: &AccountInfo<'b>,
    auction_info: &AccountInfo<'b>,
    vault_info: &AccountInfo<'b>,
    bidder_info: &AccountInfo<'b>,
    token_vault_program_info: &AccountInfo<'b>,
    token_metadata_program_info: &AccountInfo<'b>,
    token_program_info: &AccountInfo<'b>,
    payer_info: &AccountInfo<'b>,
    safety_deposit_info: &AccountInfo<'b>,
    safety_deposit_token_store_info: &AccountInfo<'b>,
    system_info: &AccountInfo<'b>,
    rent_info: &AccountInfo<'b>,
    auction_manager_bump: u8,
    me_supply: u64,
    accounts: &V2Accounts<'a, 'b>,
) -> ProgramResult {
    let actual_edition = me_supply
        .checked_add(1)
        .ok_or(MetaplexError::NumericalOverflowError)?;

    let signer_seeds = &[
        PREFIX.as_bytes(),
        auction_info.key.as_ref(),
        &[auction_manager_bump],
    ];
    mint_edition(
        token_metadata_program_info,
        token_vault_program_info,
        accounts.new_metadata_account_info,
        accounts.new_edition_account_info,
        accounts.master_edition_account_info,
        accounts.edition_marker_info,
        accounts.mint_info,
        accounts.mint_authority_info,
        payer_info,
        auction_manager_info,
        safety_deposit_token_store_info,
        safety_deposit_info,
        vault_info,
        bidder_info,
        accounts.metadata_account_info,
        token_program_info,
        system_info,
        rent_info,
        actual_edition,
        signer_seeds,
    )?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn charge_for_participation<'a>(
    bidder_token_account_info: &AccountInfo<'a>,
    accept_payment_info: &AccountInfo<'a>,
    transfer_authority_info: &AccountInfo<'a>,
    token_program_info: &AccountInfo<'a>,
    safety_deposit_config_info: &AccountInfo<'a>,
    win_index: Option<usize>,
    config: &ParticipationConfigV2,
    auction_manager_bump: u8,
    auction_manager: &mut Box<dyn AuctionManager>,
    bidder_token: &Account,
    bidder_metadata: &BidderMetadata,
) -> ProgramResult {
    let auction_key = auction_manager.auction();
    let signer_seeds = &[
        PREFIX.as_bytes(),
        auction_key.as_ref(),
        &[auction_manager_bump],
    ];

    let mut price: u64 = 0;
    if win_index.is_none() {
        if let Some(fixed_price) = config.fixed_price {
            price = fixed_price;
        } else if config.non_winning_constraint == NonWinningConstraint::GivenForBidPrice {
            price = bidder_metadata.last_bid;
        }
    }

    if bidder_token.amount.saturating_sub(price) < 0 as u64 {
        return Err(MetaplexError::NotEnoughBalanceForParticipation.into());
    }

    if price > 0 {
        auction_manager.add_to_collected_payment(safety_deposit_config_info, price)?;

        spl_token_transfer(
            bidder_token_account_info.clone(),
            accept_payment_info.clone(),
            price,
            transfer_authority_info.clone(),
            signer_seeds,
            token_program_info.clone(),
        )?;
    }
    Ok(())
}

#[allow(clippy::unnecessary_cast)]
#[allow(clippy::absurd_extreme_comparisons)]
pub fn process_redeem_participation_bid(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    legacy: bool,
    user_provided_win_index: Option<u64>,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let auction_manager_info = next_account_info(account_info_iter)?;
    let safety_deposit_token_store_info = next_account_info(account_info_iter)?;
    let destination_info = next_account_info(account_info_iter)?;
    let bid_redemption_info = next_account_info(account_info_iter)?;
    let safety_deposit_info = next_account_info(account_info_iter)?;
    let vault_info = next_account_info(account_info_iter)?;
    let safety_deposit_config_info = next_account_info(account_info_iter)?;
    let auction_info = next_account_info(account_info_iter)?;
    let bidder_metadata_info = next_account_info(account_info_iter)?;
    let bidder_info = next_account_info(account_info_iter)?;
    let payer_info = next_account_info(account_info_iter)?;
    let token_program_info = next_account_info(account_info_iter)?;
    let token_vault_program_info = next_account_info(account_info_iter)?;
    let token_metadata_program_info = next_account_info(account_info_iter)?;
    let store_info = next_account_info(account_info_iter)?;
    let system_info = next_account_info(account_info_iter)?;
    let rent_info = next_account_info(account_info_iter)?;

    let mut legacy_accounts: Option<LegacyAccounts> = None;
    let mut v2_accounts: Option<V2Accounts> = None;

    let transfer_authority_info = next_account_info(account_info_iter)?;
    let accept_payment_info = next_account_info(account_info_iter)?;
    let bidder_token_account_info = next_account_info(account_info_iter)?;
    let auction_extended_info: Option<&AccountInfo>;

    if legacy {
        legacy_accounts = Some(LegacyAccounts {
            participation_printing_holding_account_info: next_account_info(account_info_iter)?,
        });
        auction_extended_info = None;
    } else {
        let v2_accounts_base = V2Accounts {
            prize_tracking_ticket_info: next_account_info(account_info_iter)?,
            new_metadata_account_info: next_account_info(account_info_iter)?,
            new_edition_account_info: next_account_info(account_info_iter)?,
            master_edition_account_info: next_account_info(account_info_iter)?,
            mint_info: next_account_info(account_info_iter)?,
            edition_marker_info: next_account_info(account_info_iter)?,
            mint_authority_info: next_account_info(account_info_iter)?,
            metadata_account_info: next_account_info(account_info_iter)?,
            auction_extended_info: next_account_info(account_info_iter)?,
        };
        auction_extended_info = Some(v2_accounts_base.auction_extended_info);
        v2_accounts = Some(v2_accounts_base);
    }

    let CommonRedeemReturn {
        mut auction_manager,
        redemption_bump_seed,
        cancelled,
        rent: _rent,
        win_index,
        token_metadata_program: _t,
    } = common_redeem_checks(CommonRedeemCheckArgs {
        program_id,
        auction_manager_info,
        safety_deposit_token_store_info,
        destination_info,
        bid_redemption_info,
        safety_deposit_info,
        vault_info,
        auction_info,
        auction_extended_info,
        bidder_metadata_info,
        bidder_info,
        token_program_info,
        token_vault_program_info,
        token_metadata_program_info,
        rent_info,
        store_info,
        safety_deposit_config_info: Some(safety_deposit_config_info),
        is_participation: true,
        user_provided_win_index: Some(match user_provided_win_index {
            Some(val) => Some(val as usize),
            None => None,
        }),
        overwrite_win_index: None,
        assert_bidder_signer: legacy,
        ignore_bid_redeemed_item_check: false,
    })?;

    let bidder_metadata = BidderMetadata::from_account_info(bidder_metadata_info)?;

    let config: ParticipationConfigV2 =
        auction_manager.get_participation_config(safety_deposit_config_info)?;

    assert_owned_by(accept_payment_info, token_program_info.key)?;
    assert_owned_by(bidder_token_account_info, token_program_info.key)?;

    let bidder_token: Account = assert_initialized(bidder_token_account_info)?;

    if bidder_token.mint != AuctionData::get_token_mint(auction_info) {
        return Err(MetaplexError::AcceptPaymentMintMismatch.into());
    }

    if *accept_payment_info.key != auction_manager.accept_payment() {
        return Err(MetaplexError::AcceptPaymentMismatch.into());
    }

    let mut gets_participation =
        config.non_winning_constraint != NonWinningConstraint::NoParticipationPrize;

    if !cancelled {
        if AuctionData::get_is_winner(auction_info, bidder_info.key).is_some() {
            // Okay, so they placed in the auction winning prizes section!
            gets_participation =
                config.winner_constraint == WinningConstraint::ParticipationPrizeGiven;
        }
    }

    let bump_seed = assert_derivation(
        program_id,
        auction_manager_info,
        &[PREFIX.as_bytes(), &auction_manager.auction().as_ref()],
    )?;

    if gets_participation {
        if let Some(accounts) = legacy_accounts {
            let auction_key = auction_manager.auction();
            let mint_seeds = &[PREFIX.as_bytes(), auction_key.as_ref(), &[bump_seed]];

            legacy_validation(token_program_info, &auction_manager, &accounts)?;
            spl_token_transfer(
                accounts.participation_printing_holding_account_info.clone(),
                destination_info.clone(),
                1,
                auction_manager_info.clone(),
                mint_seeds,
                token_program_info.clone(),
            )?;
        } else if let Some(accounts) = v2_accounts {
            let me_supply = get_supply_off_master_edition(accounts.master_edition_account_info)?;
            v2_validation(
                program_id,
                auction_manager_info,
                store_info,
                vault_info,
                payer_info,
                token_program_info,
                system_info,
                rent_info,
                bidder_info,
                accounts.master_edition_account_info,
                destination_info,
                auction_info,
                &config,
                &accounts,
            )?;

            v2_transfer(
                auction_manager_info,
                auction_info,
                vault_info,
                bidder_info,
                token_vault_program_info,
                token_metadata_program_info,
                token_program_info,
                payer_info,
                safety_deposit_info,
                safety_deposit_token_store_info,
                system_info,
                rent_info,
                bump_seed,
                me_supply,
                &accounts,
            )?;
        }

        charge_for_participation(
            bidder_token_account_info,
            accept_payment_info,
            transfer_authority_info,
            token_program_info,
            safety_deposit_config_info,
            win_index,
            &config,
            bump_seed,
            &mut auction_manager,
            &bidder_token,
            &bidder_metadata,
        )?;
    } else {
        return Err(MetaplexError::NotEligibleForParticipation.into());
    }

    common_redeem_finish(CommonRedeemFinishArgs {
        program_id,
        auction_manager,
        auction_manager_info,
        bidder_metadata_info,
        rent_info,
        system_info,
        payer_info,
        bid_redemption_info,
        vault_info,
        safety_deposit_config_info: Some(safety_deposit_config_info),
        winning_index: win_index,
        redemption_bump_seed,
        bid_redeemed: false,
        participation_redeemed: true,
        winning_item_index: None,
        overwrite_win_index: None,
    })?;
    Ok(())
}
