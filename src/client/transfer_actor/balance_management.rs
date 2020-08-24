use safe_nd::{
    Cmd, DebitAgreementProof, Event, Money, PublicKey, Query, QueryResponse, TransferCmd,
    TransferQuery,
};
use safe_transfers::{ActorEvent, TransferInitiated};

use crate::client::Client;
use crate::errors::CoreError;

use log::{debug, info, trace};

/// Handle all Money transfers and Write API requests for a given ClientId.
impl Client {
    /// Get the account balance without querying the network
    pub async fn get_local_balance(&self) -> Money {
        info!("Retrieving actor's local balance.");
        self.transfer_actor.lock().await.balance()
    }

    /// Handle a validation event.
    pub async fn handle_validation_event(
        &mut self,
        event: Event,
    ) -> Result<Option<DebitAgreementProof>, CoreError> {
        debug!("Handling validation event: {:?}", event);
        let validation = match event {
            Event::TransferValidated { event, .. } => event,
            _ => {
                return Err(CoreError::from(format!(
                    "Unexpected event received at TransferActor, {:?}",
                    event
                )))
            }
        };
        let mut actor = self.transfer_actor.lock().await;
        let transfer_validation = match actor.receive(validation) {
            Ok(Some(validation)) => validation,
            Ok(None) => return Ok(None),
            Err(error) => {
                if !error
                    .clone()
                    .to_string()
                    .contains("Already received validation")
                {
                    return Err(CoreError::from(error));
                }

                return Ok(None);
            }
        };

        actor.apply(ActorEvent::TransferValidationReceived(
            transfer_validation.clone(),
        ))?;

        Ok(transfer_validation.proof)
    }

    /// Get the current balance for this TransferActor PK (by default) or any other...
    pub async fn get_balance_from_network(
        &mut self,
        pk: Option<PublicKey>,
    ) -> Result<Money, CoreError> {
        info!("Getting balance for {:?} or self", pk);
        let identity = self.full_id().await;
        let public_key = pk.unwrap_or(*identity.public_key());

        // let request = Self::wrap_money_request(Query::GetBalance(public_key));

        let msg_contents = Query::Transfer(TransferQuery::GetBalance(public_key));

        let message = Self::create_query_message(msg_contents);

        match self.connection_manager.send_query(&message).await? {
            QueryResponse::GetBalance(balance) => balance.map_err(CoreError::from),
            _ => Err(CoreError::from("Unexpected response when querying balance")),
        }
    }

    /// Send money
    pub async fn send_money(&mut self, to: PublicKey, amount: Money) -> Result<(), CoreError> {
        info!("Sending money");

        // first make sure our balance  history is up to date
        self.get_history().await?;

        // let mut actor = self.transfer_actor.lock().await;

        println!(
            "Debits form our actor at send: {:?}",
            self.transfer_actor.lock().await.debits_since(0)
        );

        let signed_transfer = self
            .transfer_actor
            .lock()
            .await
            .transfer(amount, to)?
            .ok_or_else(|| CoreError::from("No transfer generated by the actor."))?
            .signed_transfer;

        println!(
            "Signed transfer for send money: {:?}",
            signed_transfer.transfer
        );
        let msg_contents = Cmd::Transfer(TransferCmd::ValidateTransfer(signed_transfer.clone()));

        let message = Self::create_cmd_message(msg_contents);

        self.transfer_actor
            .lock()
            .await
            .apply(ActorEvent::TransferInitiated(TransferInitiated {
                signed_transfer,
            }))?;

        let debit_proof: DebitAgreementProof = self.await_validation(&message).await?;

        // Register the transfer on the network.
        let msg_contents = Cmd::Transfer(TransferCmd::RegisterTransfer(debit_proof.clone()));

        let message = Self::create_cmd_message(msg_contents);
        trace!(
            "Debit proof received and to be sent in RegisterTransfer req: {:?}",
            debit_proof
        );

        let _ = self.connection_manager.send_cmd(&message).await?;

        let mut actor = self.transfer_actor.lock().await;
        // First register with local actor, then reply.
        let register_event = actor
            .register(debit_proof)?
            .ok_or_else(|| CoreError::from("No transfer event to register locally"))?;

        actor.apply(ActorEvent::TransferRegistrationSent(register_event.clone()))?;

        Ok(())
    }
}

// --------------------------------
// Tests
// ---------------------------------

// TODO: Do we need "new" to actually instantiate with a transfer?...
#[cfg(all(test, feature = "simulated-payouts"))]
mod tests {

    use super::*;
    use crate::crypto::shared_box;
    use std::str::FromStr;
    use crate::utils::{generate_random_vector, test_utils::calculate_new_balance};
    use safe_nd::{Error as SndError, Money, PublicBlob, Blob};

    #[tokio::test]
    #[cfg(feature = "simulated-payouts")]
    async fn transfer_actor_can_send_money_and_thats_reflected_locally() -> Result<(), CoreError> {
        let (sk, pk) = shared_box::gen_bls_keypair();
        let (sk2, pk2) = shared_box::gen_bls_keypair();

        let pk = PublicKey::Bls(pk);
        let pk2 = PublicKey::Bls(pk2);

        let mut initial_actor = Client::new(Some(sk.clone())).await?;

        let _ = initial_actor.send_money(pk2, Money::from_str("1")?).await?;

        // initial 10 on creation from farming simulation minus 1
        assert_eq!(
            initial_actor.get_local_balance().await,
            Money::from_str("9")?
        );

        assert_eq!(
            initial_actor.get_balance_from_network(None).await?,
            Money::from_str("9")?
        );

        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "simulated-payouts")]
    async fn transfer_actor_can_send_several_transfers_and_thats_reflected_locally(
    ) -> Result<(), CoreError> {
        let (sk, pk) = shared_box::gen_bls_keypair();
        let (sk2, pk2) = shared_box::gen_bls_keypair();

        let pk = PublicKey::Bls(pk);
        let pk2 = PublicKey::Bls(pk2);

        let mut client = Client::new(Some(sk.clone())).await?;

        println!("starting.....");
        let _ = client.send_money(pk2, Money::from_str("1")?).await?;

        // initial 10 on creation from farming simulation minus 1
        assert_eq!(client.get_local_balance().await, Money::from_str("9")?);

        assert_eq!(
            client.get_balance_from_network(None).await?,
            Money::from_str("9")?
        );

        println!("FIRST DONE!!!!!!!!!!!!!!");

        let _ = client.send_money(pk2, Money::from_str("2")?).await?;

        // initial 10 on creation from farming simulation minus 3
        Ok(assert_eq!(
            client.get_local_balance().await,
            Money::from_str("7")?
        ))
    }

    // TODO: do we want to be able to send 0 transfer reqs? This should probably be an actor side check if not
    #[tokio::test]
    #[cfg(feature = "simulated-payouts")]
    async fn transfer_actor_cannot_send_0_money_req() -> Result<(), CoreError> {
        let (sk, cm) = shared_box::gen_bls_keypair();
        let (sk2, _cm) = shared_box::gen_bls_keypair();

        let mut initial_actor = TransferActor::new(sk.clone(), cm.clone()).await?;

        let res = initial_actor
            .send_money(sk2.public_key(), Money::from_str("0")?)
            .await?;

        println!("res to send 0: {:?}", res);

        // initial 10 on creation from farming simulation minus 1
        assert_eq!(
            initial_actor.get_local_balance().await,
            Money::from_str("10")?
        );

        assert_eq!(
            initial_actor.get_balance_from_network(None).await?,
            Money::from_str("10")?
        );

        Ok(())
    }


    // 1. Create a client A and allocate some test safecoin to it.
    // 2. Get the balance and verify it.
    // 3. Create another client B with a wallet holding some safecoin.
    // 4. Transfer some money from client B to client A and verify the new balance.
    // 5. Try to do a coin transfer without enough funds, it should return `InsufficientBalance`
    // 6. Try to do a coin transfer with the amount set to 0, it should return `InvalidOperation`
    #[cfg(feature = "simulated-payouts")]
    pub async fn balance_transfers_between_clients() -> Result<(), CoreError> {
        let mut client = Client::new(None).await?;
        let mut receiving_client = Client::new(None).await?;

        let wallet1 = receiving_client.public_key().await;

        client
            .test_simulate_farming_payout_client(Money::from_str("100.0")?)
            .await
            ?;

        let balance = client.get_balance(None).await?;
        assert_eq!(balance, Money::from_str("110")?); // 10 coins added automatically w/ farming sim on client init. 
        let init_bal = Money::from_str("10")?;
        let orig_balance = client.get_balance(None).await?;
        let _ = client
            .send_money(wallet1, Money::from_str("5.0")?)
            .await
            ?;
        let new_balance = client.get_balance(None).await?;
        assert_eq!(
            new_balance,
            orig_balance.checked_sub(Money::from_str("5.0")?).ok_or(CoreError::from("Invalid checked sub in test"))?,
        );

        let res = client
            .send_money(wallet1, Money::from_str("5000")?)
            .await;
        match res {
            Err(CoreError::DataError(SndError::InsufficientBalance)) => (),
            res => panic!("Unexpected result: {:?}", res),
        };
        // Check if money is refunded
        let balance = client.get_balance(None).await?;
        let receiving_balance = receiving_client.get_balance(None).await?;

        let expected =
            calculate_new_balance(init_bal, Some(1), Some(Money::from_str("5")?));
        assert_eq!(balance, expected);

        assert_eq!(receiving_balance, Money::from_str("5015")?); // 500 + 5 + initial 10


        Ok(())
    }

    #[cfg(feature = "simulated-payouts")]
    pub async fn cannot_write_with_insufficient_balance() -> Result<(), CoreError> {
        let mut client = Client::new(None).await?;
        let mut receiving_client = Client::new(None).await?;

        let wallet1 = receiving_client.public_key().await;

      
        let _ = client
            .send_money(wallet1, Money::from_str("10")?)
            .await
            ?;
            
            
        let data = Blob::Public(PublicBlob::new(generate_random_vector::<u8>(10)));
        let res = client.store_blob(data).await;
        match res {
            Err(CoreError::DataError(SndError::InsufficientBalance)) => (),
            res => panic!(
                "Unexpected result in money transfer test, putting without balance: {:?}",
                res
            ),
        };

        Ok(())
    }
}
