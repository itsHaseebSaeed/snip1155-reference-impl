use cosmwasm_std::{
    Env, Extern, Storage, Api, Querier, 
    InitResponse, HandleResponse, Binary, to_binary, log,
    StdResult, StdError,
    HumanAddr, Uint128,
    CosmosMsg, QueryResult, 
    // debug_print, 
};
use secret_toolkit::utils::space_pad;

use crate::{
    msg::{
        InitMsg, HandleMsg, HandleAnswer, QueryMsg, QueryAnswer,
        ResponseStatus::{Success}, 
    },
    state::{
        RESPONSE_BLOCK_SIZE, BLOCK_KEY,
        ContrConf, TknInfo, MintTokenId, TokenAmount, Balance, Permission,
        contr_conf_w, tkn_info_r, 
        tkn_info_w, balances_w, balances_r, contr_conf_r, 
        store_transfer, store_mint, store_burn,
        set_receiver_hash, get_receiver_hash, write_viewing_key, read_viewing_key, get_txs,
        permission_w, permission_r,
        json_save, 
    },
    receiver::{Snip1155ReceiveMsg}, 
    vk::{
        viewing_key::{VIEWING_KEY_SIZE, ViewingKey,},
        rand::sha_256,
    }, 
    // expiration::Expiration,
    
};

/////////////////////////////////////////////////////////////////////////////////
// Init
/////////////////////////////////////////////////////////////////////////////////

pub fn init<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    msg: InitMsg,
) -> StdResult<InitResponse> {
    // save latest block info. not necessary once we migrate to CosmWasm v1.0 
    json_save(&mut deps.storage, BLOCK_KEY, &env.block)?;

    // set admin. If `has_admin` == None => no admin. 
    // If `has_admin` == true && msg.admin == None => admin is the instantiator
    let admin = match msg.has_admin {
        false => None,
        true => match msg.admin {
            Some(i) => Some(i),
            None => Some(env.message.sender.clone()),
        },
    };
    
    // create contract config -- save later
    let prng_seed: Vec<u8> = sha_256(base64::encode(msg.entropy).as_bytes()).to_vec();
    let mut config = ContrConf { 
        admin, 
        minters: msg.minters,
        tx_cnt: 0u64,
        prng_seed,
    };

    // set initial balances
    for initial_token in msg.initial_tokens {
        exec_mint_token_id(
            deps, 
            &env,
            &mut config,
            initial_token,
            None,
        )?;
    }

    // save contract config -- where tx_cnt would have increased post initial balances
    contr_conf_w(&mut deps.storage).save(&config)?;
    
    Ok(InitResponse::default())
}

/////////////////////////////////////////////////////////////////////////////////
// Handles
/////////////////////////////////////////////////////////////////////////////////

pub fn handle<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    msg: HandleMsg,
) -> StdResult<HandleResponse> {
    // allows approx latest block info to be available for queries. Important to enforce
    // allowance expiration. Remove this after BlockInfo becomes available to queries
    json_save(&mut deps.storage, BLOCK_KEY, &env.block)?;

    let response = match msg {
        HandleMsg::MintTokenIds {
            initial_tokens,
            memo,
            padding: _,
         } => try_mint_token_ids(
            deps,
            env,
            initial_tokens,
            memo,
        ),
        HandleMsg::MintTokens {
            mint_tokens,
            memo,
            padding: _
         } => try_mint_tokens(
            deps, 
            env,
            mint_tokens,
            memo
        ),
        HandleMsg::BurnTokens { 
            burn_tokens, 
            memo, 
            padding: _ 
        } => try_burn_tokens(
            deps, 
            env, 
            burn_tokens, 
            memo
        ),
        HandleMsg::Transfer { 
            token_id,
            from,
            recipient, 
            amount,
            memo,
            padding: _,
        } => try_transfer(
            deps,
            env,
            token_id,
            from,
            recipient,
            amount,
            memo,
        ),
        HandleMsg::Send { 
            token_id, 
            from, 
            recipient, 
            recipient_code_hash, 
            amount, 
            msg, 
            memo, 
            padding: _, 
        } => try_send(
            deps,
            env,
            TokenAmount { token_id, balances: [Balance {address: from, amount }].to_vec()},
            recipient,
            recipient_code_hash,
            msg,
            memo,
        ),
        HandleMsg::GivePermission {
            address,
            token_id,
            view_owner,
            view_private_metadata,
            transfer,
            padding: _,
        } => try_give_permission(
            deps,
            env,
            address,
            token_id,
            view_owner,
            view_private_metadata,
            transfer,
            ),
        HandleMsg::RegisterReceive { 
            code_hash, 
            padding: _, 
        } => try_register_receive(
            deps, 
            env, 
            code_hash
        ),
        HandleMsg::CreateViewingKey { 
            entropy, 
            padding: _ 
        } => try_create_key(
            deps, 
            env, 
            entropy
        ),
        HandleMsg::SetViewingKey { 
            key, 
            padding: _ 
        } => try_set_key(
            deps, 
            env, 
            key
        ),        
    };
    pad_response(response)
}

fn try_mint_token_ids<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    initial_tokens: Vec<MintTokenId>,
    memo: Option<String>,
) -> StdResult<HandleResponse> {
    // check if sender is a minter
    verify_minter(&deps.storage, &env)?;

    // mint new token_ids
    let mut config = contr_conf_r(&deps.storage).load()?;
    for initial_token in initial_tokens {
        exec_mint_token_id(
            deps, 
            &env,
            &mut config,
            initial_token, 
            memo.clone(),
        )?;
    } 

    contr_conf_w(&mut deps.storage).save(&config)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::MintTokenIds { status: Success })?)
    })
}

fn try_mint_tokens<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    mint_tokens: Vec<TokenAmount>,
    memo: Option<String>,
) -> StdResult<HandleResponse> {
    // check if sender is a minter
    verify_minter(&deps.storage, &env)?;

    // mint tokens
    let mut config = contr_conf_r(&deps.storage).load()?;
    for mint_token in mint_tokens {
        let token_info_op = tkn_info_r(&deps.storage).may_load(mint_token.token_id.as_bytes())?;
    
        if token_info_op.is_none() {
            return Err(StdError::generic_err(
                "token_id does not exist. Cannot mint non-existent `token_ids`.
                Use `mint_token_ids` to create tokens on new `token_ids`"
            ))
        }

        // add balances
        for add_balance in mint_token.balances {
            exec_change_balance(
                &mut deps.storage, 
                &mint_token.token_id, 
                None, 
                Some(&add_balance.address), 
                &add_balance.amount, 
                &token_info_op.clone().unwrap()
            )?;

            // store mint_token
            store_mint(
                &mut deps.storage, 
                &mut config, 
                &env.block,
                &mint_token.token_id,
                deps.api.canonical_address(&env.message.sender)?, 
                deps.api.canonical_address(&add_balance.address)?, 
                add_balance.amount, 
                memo.clone()
            )?;
        }
    }

    contr_conf_w(&mut deps.storage).save(&config)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::MintTokens { status: Success })?)
    })
}

// in the base specifications, this function can be performed by token owner only
fn try_burn_tokens<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    burn_tokens: Vec<TokenAmount>,
    memo: Option<String>,
) -> StdResult<HandleResponse> {
    let mut config = contr_conf_r(&deps.storage).load()?;
    
    // burn tokens
    for burn_token in burn_tokens {
        let token_info_op = tkn_info_r(&deps.storage).may_load(burn_token.token_id.as_bytes())?;
    
        if token_info_op.is_none() {
            return Err(StdError::generic_err(
                "token_id does not exist. Cannot burn non-existent `token_ids`.
                Use `mint_token_ids` to create tokens on new `token_ids`"
            ))
        }

        let token_info = token_info_op.clone().unwrap();

        if !token_info.token_config.enable_burn {
            return Err(StdError::generic_err(
                "burn is not enabled for this token_id"
            ))
        }

        // remove balances
        for rem_balance in burn_token.balances {
            // in base specification, burner MUST be the owner
            if rem_balance.address != env.message.sender {
                return Err(StdError::generic_err(format!(
                    "you do not have permission to burn {} tokens from address {}",
                    rem_balance.amount, rem_balance.address
                )))
            }

            exec_change_balance(
                &mut deps.storage, 
                &burn_token.token_id, 
                Some(&rem_balance.address), 
                None,
                &rem_balance.amount, 
                &token_info
            )?;

            // store burn_token
            store_burn(
                &mut deps.storage, 
                &mut config, 
                &env.block,
                &burn_token.token_id,
                // in base specification, burner MUST be the owner
                None, 
                deps.api.canonical_address(&rem_balance.address)?, 
                rem_balance.amount, 
                memo.clone()
            )?;
        }
    }

    contr_conf_w(&mut deps.storage).save(&config)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::BurnTokens { status: Success })?)
    })
}

fn try_transfer<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    token_id: String,
    from: HumanAddr,
    recipient: HumanAddr,
    amount: Uint128,
    memo: Option<String>,
) -> StdResult<HandleResponse> {
    impl_transfer(
        deps, 
        &env, 
        &token_id, 
        &from, 
        &recipient, 
        amount, 
        memo
    )?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::Transfer { status: Success })?)
    })
}

fn try_send<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    from_amount: TokenAmount, 
    recipient: HumanAddr,
    recipient_code_hash: Option<String>,
    msg: Option<Binary>,
    memo: Option<String>,
) -> StdResult<HandleResponse> {
    // done to avoid having too many arguments
    let token_id = from_amount.token_id;
    let from = &from_amount.balances[0].address;
    let amount = from_amount.balances[0].amount;

    // implements transfer of tokens
    impl_transfer(
        deps, 
        &env, 
        &token_id, 
        from, 
        &recipient, 
        amount, 
        memo.clone()
    )?;

    // create cosmos message
    let mut messages = vec![];
    let sender = env.message.sender;
    try_add_receiver_api_callback(
        &deps.storage,
        &mut messages,
        recipient,
        recipient_code_hash,
        msg,
        sender,
        token_id,
        from.to_owned(),
        amount,
        memo,
    )?;

    Ok(HandleResponse {
        messages,
        log: vec![],
        data: Some(to_binary(&HandleAnswer::Send { status: Success })?)
    })
}

/// does not check if `token_id` exists so attacker cannot easily figure out if
/// a `token_id` has been created 
pub fn try_give_permission<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    address: HumanAddr,
    token_id: String,
    view_owner: Option<bool>,
    view_private_metadata: Option<bool>,
    transfer: Option<Uint128>, 
) -> StdResult<HandleResponse> {
    // may_load current permission
    let address_key = to_binary(&address)?;
    let permission_op = permission_r(
        &deps.storage,
        &env.message.sender,
        &token_id,
    ).may_load(address_key.as_slice())?;

    // start with default permission if no permission has been created yet
    let mut permission = match permission_op {
        Some(i) => i,
        None => Permission::default(),
    };
    
    // modify permission struct
    if let Some(i) = view_owner { permission.view_owner_perm = i };
    if let Some(i) = view_private_metadata { permission.view_pr_metadata_perm = i };
    if let Some(i) = transfer { permission.trfer_allowance_perm = i };
    
    // save new permission
    permission_w(&mut deps.storage, &env.message.sender, &token_id)
        .save(address_key.as_slice(), &permission)?;

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::GivePermission { status: Success })?),
    })
}

fn try_register_receive<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    code_hash: String,
) -> StdResult<HandleResponse> {
    set_receiver_hash(&mut deps.storage, &env.message.sender, code_hash);
    let res = HandleResponse {
        messages: vec![],
        log: vec![log("register_status", "success")],
        data: Some(to_binary(&HandleAnswer::RegisterReceive {
            status: Success,
        })?),
    };
    Ok(res)
}

fn try_create_key<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    entropy: String,
) -> StdResult<HandleResponse> {
    // let constants = ReadonlyConfig::from_storage(&deps.storage).constants()?;
    let contr_conf = contr_conf_r(&deps.storage).load()?;
    let prng_seed = contr_conf.prng_seed;

    let key = ViewingKey::new(&env, &prng_seed, (&entropy).as_ref());

    let message_sender = deps.api.canonical_address(&env.message.sender)?;
    write_viewing_key(&mut deps.storage, &message_sender, &key);

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::CreateViewingKey { key })?),
    })
}

fn try_set_key<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: Env,
    key: String,
) -> StdResult<HandleResponse> {
    let vk = ViewingKey(key);

    let message_sender = deps.api.canonical_address(&env.message.sender)?;
    write_viewing_key(&mut deps.storage, &message_sender, &vk);

    Ok(HandleResponse {
        messages: vec![],
        log: vec![],
        data: Some(to_binary(&HandleAnswer::SetViewingKey { status: Success })?),
    })
}


/////////////////////////////////////////////////////////////////////////////////
// Private functions
/////////////////////////////////////////////////////////////////////////////////

fn pad_response(
    response: StdResult<HandleResponse>
) -> StdResult<HandleResponse> {
    response.map(|mut response| {
        response.data = response.data.map(|mut data| {
            space_pad(&mut data.0, RESPONSE_BLOCK_SIZE);
            data
        });
        response
    })
}

/// verifies if sender is a minter
fn verify_minter<S: Storage>(
    storage: &S,
    env: &Env
) -> StdResult<()> {
    // check if sender is a minter
    let minters = contr_conf_r(storage).load()?.minters;
    if !minters.contains(&env.message.sender) {
        return Err(StdError::generic_err(
            "Only minters are allowed to mint",
        ));
    }
    Ok(())
}

/// checks if `token_id` is available (ie: not yet created), then creates new `token_id` and initial balances
fn exec_mint_token_id<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: &Env,
    config: &mut ContrConf,
    initial_token: MintTokenId,
    memo: Option<String>,
) -> StdResult<()> {
    // check: token_id has not been created yet
    if tkn_info_r(&deps.storage).may_load(initial_token.token_info.token_id.as_bytes())?.is_some() {
        return Err(StdError::generic_err("token_id already exists. Try a different id String"))
    }

    // check: token_id is an NFT => cannot create more than one
    if initial_token.token_info.is_nft {
        if initial_token.balances.len() > 1 {
            return Err(StdError::generic_err(format!(
                "token_id {} is an NFT; there can only be one NFT. Balances should only have one address",
                initial_token.token_info.token_id
            )))
        } else if initial_token.balances[0].amount != Uint128(1) {
            return Err(StdError::generic_err(format!(
                "token_id {} is an NFT; there can only be one NFT. Balances.amount must == 1",
                initial_token.token_info.token_id
            )))
        }
    }

    // crate and save new token info
    tkn_info_w(&mut deps.storage).save(initial_token.token_info.token_id.as_bytes(), &initial_token.token_info)?;

    // set initial balances and store mint history
    for balance in initial_token.balances {
        balances_w(&mut deps.storage, &initial_token.token_info.token_id)
        .save(to_binary(&balance.address)?.as_slice(), &balance.amount)?;

        // store mint_token_id
        store_mint(
            &mut deps.storage, 
            config, 
            &env.block,
            &initial_token.token_info.token_id, 
            deps.api.canonical_address(&env.message.sender)?, 
            deps.api.canonical_address(&balance.address)?, 
            balance.amount, 
            memo.clone()
        )?;
    }

    Ok(())
}

/// Implements transfer of tokens, and saves the transfer history. Used by
/// `Transfer` and `Send` messages
fn impl_transfer<S: Storage, A: Api, Q: Querier>(
    deps: &mut Extern<S, A, Q>,
    env: &Env,
    token_id: &str,
    from: &HumanAddr,
    recipient: &HumanAddr,
    amount: Uint128,
    memo: Option<String>,
) -> StdResult<()> {
    let sender_bin = to_binary(&env.message.sender)?;
    let sender_u8 = sender_bin.as_slice(); 
    // check if `from` == message sender || has enough allowance to send tokens
    let permission_op = permission_r(&deps.storage, from, token_id)
        .may_load(sender_u8)?;

    // perform allowance check, and may reduce allowance 
    let mut throw_err = false;
    if from != &env.message.sender {
        match permission_op {
            // no permission given
            None => throw_err = true,
            // not enough allowance to transfer amount
            Some(perm) if perm.trfer_allowance_perm < amount => return Err(StdError::generic_err(format!(
                "Insufficient transfer allowance: {}", perm.trfer_allowance_perm
            ))),
            // reduce allowance
            Some(mut perm) if perm.trfer_allowance_perm >= amount => {
                let new_allowance = Uint128(perm.trfer_allowance_perm.u128().checked_sub(amount.u128()).expect("something strange happened"));
                perm.trfer_allowance_perm = new_allowance;
                permission_w(&mut deps.storage, from, token_id).save(sender_u8, &perm)?;
            },
            Some(_) => unreachable!()
        }
    }

    // check that token_id exists
    let token_info_op = tkn_info_r(&deps.storage).may_load(token_id.as_bytes())?;
    if token_info_op.is_none() { throw_err = true }

    // combined error message for no token_id or no permission given in one place to make it harder to identify if token_id already exists
    match throw_err {
        true => return Err(StdError::generic_err("These tokens do not exist or you have no permission to transfer")),
        false => (),
    }

    // transfer tokens
    exec_change_balance(
        &mut deps.storage, 
        token_id, 
        Some(from), 
        Some(recipient), 
        &amount, 
        &token_info_op.unwrap()
    )?;

    // store transaction
    let mut config = contr_conf_r(&deps.storage).load()?;
    store_transfer(
        &mut deps.storage, 
        &mut config, 
        &env.block, 
        token_id, 
        deps.api.canonical_address(from)?, 
        None, 
        deps.api.canonical_address(recipient)?, 
        amount, 
        memo
    )?;
    contr_conf_w(&mut deps.storage).save(&config)?;

    Ok(())
}

/// change token balance of an existing `token_id`. 
/// 
/// Should check that `token_id` already exists before calling this function, which is not done
/// explicitly in this function.
/// * If `remove_from` == None: minted new tokens.
/// * If `add_to` == None: burn tokens.
/// * If is_nft == true, then `remove_from` MUST be Some(_).
fn exec_change_balance<S: Storage>(
    storage: &mut S,
    token_id: &str,
    remove_from: Option<&HumanAddr>,
    add_to: Option<&HumanAddr>,
    amount: &Uint128,
    token_info: &TknInfo,
) -> StdResult<()> {
    // check whether token_id is an NFT => cannot mint
    if token_info.is_nft && remove_from == None {
        return Err(StdError::generic_err("NFTs can only be minted once using `mint_token_ids`"))
    }

    // check whether token_id is an NFT => assert!(amount == 1). 
    if token_info.is_nft && amount != &Uint128(1) {
        return Err(StdError::generic_err("NFT amount must == 1"))
    }

    // remove balance
    if let Some(from) = remove_from {
        let from_existing_bal = balances_r(storage, token_id).load(to_binary(&from)?.as_slice())?;
        let from_new_amount_op = from_existing_bal.u128().checked_sub(amount.u128());
        if from_new_amount_op.is_none() {
            return Err(StdError::generic_err("insufficient funds"))
        }    
        balances_w(storage, token_id)
        .save(to_binary(&from)?.as_slice(), &Uint128(from_new_amount_op.unwrap()))?;
    }

    // add balance
    if let Some(to) = add_to {
        let to_existing_bal_op = balances_r(storage, token_id).may_load(to_binary(&to)?.as_slice())?; 
        let to_existing_bal = match to_existing_bal_op {
            Some(i) => i,
            // if `to` address has no balance yet, initiate zero balance
            None => Uint128(0),
        };
        let to_new_amount_op = to_existing_bal.u128().checked_add(amount.u128());
        if to_new_amount_op.is_none() {
            return Err(StdError::generic_err("recipient will become too rich. Total tokens exceeds 2^128"))
        }

        // save new balances
        balances_w(storage, token_id)
        .save(to_binary(&to)?.as_slice(), &Uint128(to_new_amount_op.unwrap()))?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn try_add_receiver_api_callback<S: Storage>(
    storage: &S,
    messages: &mut Vec<CosmosMsg>,
    recipient: HumanAddr,
    recipient_code_hash: Option<String>,
    msg: Option<Binary>,
    sender: HumanAddr,
    token_id: String,
    from: HumanAddr,
    amount: Uint128,
    memo: Option<String>,
) -> StdResult<()> {
    if let Some(receiver_hash) = recipient_code_hash {
        let receiver_msg = Snip1155ReceiveMsg::new(sender, token_id, from, amount, memo, msg);
        let callback_msg = receiver_msg.into_cosmos_msg(receiver_hash, recipient)?;

        messages.push(callback_msg);
        return Ok(());
    }

    let receiver_hash = get_receiver_hash(storage, &recipient);
    if let Some(receiver_hash) = receiver_hash {
        let receiver_hash = receiver_hash?;
        let receiver_msg = Snip1155ReceiveMsg::new(sender, token_id, from, amount, memo, msg);
        let callback_msg = receiver_msg.into_cosmos_msg(receiver_hash, recipient)?;

        messages.push(callback_msg);
    }
    
    Ok(())
}


/////////////////////////////////////////////////////////////////////////////////
// Queries
/////////////////////////////////////////////////////////////////////////////////

pub fn query<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    msg: QueryMsg,
) -> StdResult<Binary> {
    match msg {
        QueryMsg::ContractInfo {  } => query_contract_info(deps),
        QueryMsg::Balance { .. } |
        QueryMsg::TransferHistory { .. } | 
        QueryMsg::Permission { .. }  => viewing_keys_queries(deps, msg),
    }
}

fn viewing_keys_queries<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    msg: QueryMsg,
) -> QueryResult {
    let (addresses, key) = msg.get_validation_params();

    for address in addresses {
        let canonical_addr = deps.api.canonical_address(address)?;

        let expected_key = read_viewing_key(&deps.storage, &canonical_addr);

        if expected_key.is_none() {
            // Checking the key will take significant time. We don't want to exit immediately if it isn't set
            // in a way which will allow to time the command and determine if a viewing key doesn't exist
            key.check_viewing_key(&[0u8; VIEWING_KEY_SIZE]);
        } else if key.check_viewing_key(expected_key.unwrap().as_slice()) {
            return match msg {
                QueryMsg::Balance { address, token_id, .. } => query_balance(deps, &address, token_id),
                QueryMsg::TransferHistory {
                    address,
                    page,
                    page_size,
                    ..
                } => query_transfers(deps, &address, page.unwrap_or(0), page_size),
                QueryMsg::Permission { owner, perm_address, token_id, .. } => query_permission(deps, token_id, owner, perm_address),
                _ => panic!("This query type does not require authentication"),
            };
        }
    }

    to_binary(&QueryAnswer::ViewingKeyError {
        msg: "Wrong viewing key for this address or viewing key not set".to_string(),
    })
}

fn query_contract_info<S: Storage, A: Api, Q: Querier>(
    _deps: &Extern<S, A, Q>,
) -> StdResult<Binary> {
    let info = "data".to_string();
    let response = QueryAnswer::ContractInfo { info };
    to_binary(&response)
}

fn query_balance<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    account: &HumanAddr,
    token_id: String,
) -> StdResult<Binary> {
    let address = deps.api.canonical_address(account)?;

    let amount_op = balances_r(&deps.storage, &token_id)
        .may_load(to_binary(&deps.api.human_address(&address)?)?.as_slice())?;
    let amount = match amount_op {
        Some(i) => i,
        None => Uint128(0),
    };
    let response = QueryAnswer::Balance { amount };
    to_binary(&response)
}

fn query_transfers<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    account: &HumanAddr,
    page: u32,
    page_size: u32,
) -> StdResult<Binary> {
    let address = deps.api.canonical_address(account)?;
    let (txs, total) = get_txs(&deps.storage, &address, page, page_size)?;

    let result = QueryAnswer::TransferHistory {
        txs,
        total: Some(total),
    };
    to_binary(&result)
}

fn query_permission<S: Storage, A: Api, Q: Querier>(
    deps: &Extern<S, A, Q>,
    token_id: String,
    owner: HumanAddr,
    perm_address: HumanAddr,
) -> StdResult<Binary> {
    let perm_addr_bin = to_binary(&perm_address)?;
    let perm_addr_bytes = perm_addr_bin.as_slice();
    let permission = permission_r(&deps.storage, &owner, &token_id).may_load(&perm_addr_bytes)?
        .unwrap_or_default();

    let response = QueryAnswer::Permission(permission);
    to_binary(&response)
}

