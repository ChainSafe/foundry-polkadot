
use crate::{
    substrate_node::{
       service::{AuxStore, ProvideRuntimeApi, UsageProvider, AuraApi, AuthorityId, TIMESTAMP}
    },
};
use polkadot_sdk::{
    sc_consensus::BlockImportParams,
    sc_consensus_aura::{self, CompatibleDigestItem},
    sc_consensus_manual_seal::{ConsensusDataProvider, Error},
    sp_consensus_aura::ed25519::AuthoritySignature,
    sp_consensus_babe::Slot,
    sp_inherents::InherentData,
    sp_runtime::{Digest, DigestItem, traits::Block as BlockT},
	sp_timestamp::TimestampInherentData,
};
use std::marker::PhantomData;
use std::sync::atomic::{ Ordering};
use std::sync::Arc;

/// Consensus data provider for Aura. This will always use slot 0 (used to determine the
/// index of the AURA authority from the authorities set by AURA runtimes) for the aura
/// digest since anvil-polkadot node will be the sole block author and AURA will pick
/// only its configured address, residing at index 0 in the AURA authorities set. When
/// forking from an assethub chain, we expect an assethub runtime based on AURA,
/// which will pick the author based on the slot given through the digest, which will
/// also result in picking the AURA authority from index 0.
pub struct SameSlotConsensusDataProvider<B, P> {
    _phantom: PhantomData<(B, P)>,
}

impl<B, P> SameSlotConsensusDataProvider<B, P> {
    pub fn new() -> Self {
        Self { _phantom: PhantomData }
    }
}

impl<B, P> ConsensusDataProvider<B> for SameSlotConsensusDataProvider<B, P>
where
    B: BlockT,
    P: Send + Sync,
{
    type Proof = P;

    fn create_digest(
        &self,
        _parent: &B::Header,
        _inherents: &InherentData,
    ) -> Result<Digest, Error> {
        let digest_item = <DigestItem as CompatibleDigestItem<AuthoritySignature>>::aura_pre_digest(
            Slot::default(),
        );

        Ok(Digest { logs: vec![digest_item] })
    }

    fn append_block_import(
        &self,
        _parent: &B::Header,
        _params: &mut BlockImportParams<B>,
        _inherents: &InherentData,
        _proof: Self::Proof,
    ) -> Result<(), Error> {
        Ok(())
    }
}

// Mine /// Consensus data provider for Aura. This allows to use manual-seal driven nodes to author valid
/// AURA blocks. It will inspect incoming [`InherentData`] and look for included timestamps. Based
/// on these timestamps, the [`AuraConsensusDataProvider`] will emit fitting digest items.
pub struct AuraConsensusDataProvider<B, P> {
	// slot duration
	slot_duration: sc_consensus_aura::SlotDuration,
	// phantom data for required generics
	_phantom: PhantomData<(B, P)>,
}

impl<B, P> AuraConsensusDataProvider<B, P>
where
	B: BlockT,
{
	/// Creates a new instance of the [`AuraConsensusDataProvider`], requires that `client`
	/// implements [`sp_consensus_aura::AuraApi`]
	pub fn new<C>(client: Arc<C>) -> Self
	where
		C: AuxStore + ProvideRuntimeApi<B> + UsageProvider<B>,
		C::Api: AuraApi<B, AuthorityId>,
	{
		let slot_duration = sc_consensus_aura::slot_duration(&*client)
			.expect("slot_duration is always present; qed.");

		Self { slot_duration, _phantom: PhantomData }
	}

	/// Creates a new instance of the [`AuraConsensusDataProvider`]
	pub fn new_with_slot_duration(slot_duration: sc_consensus_aura::SlotDuration) -> Self {
		Self { slot_duration, _phantom: PhantomData }
	}
}

impl<B, P> ConsensusDataProvider<B> for AuraConsensusDataProvider<B, P>
where
	B: BlockT,
	P: Send + Sync,
{
	type Proof = P;

	fn create_digest(
        &self,
        _parent: &B::Header,
        inherents: &InherentData,
    ) -> Result<Digest, Error> {
        let timestamp =
            inherents.timestamp_inherent_data()?.expect("Timestamp is always present; qed");

        print!("time da {}", timestamp);
        print!("time db {}", TIMESTAMP.load(Ordering::SeqCst));

        // we always calculate the new slot number based on the current time-stamp and the slot
        // duration.
        let digest_item = <DigestItem as CompatibleDigestItem<AuthoritySignature>>::aura_pre_digest(
            Slot::from_timestamp(timestamp, self.slot_duration),
        );

        Ok(Digest { logs: vec![digest_item] })
    }

	fn append_block_import(
		&self,
		_parent: &B::Header,
		_params: &mut BlockImportParams<B>,
		_inherents: &InherentData,
		_proof: Self::Proof,
	) -> Result<(), Error> {
		Ok(())
	}
}
