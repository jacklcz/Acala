#![cfg_attr(not(feature = "std"), no_std)]

use codec::Encode;
use frame_support::{decl_error, decl_event, decl_module, decl_storage, ensure, traits::Get};
use frame_system::{self as system, ensure_none, ensure_signed};
use orml_traits::BasicCurrency;
use primitives::Balance;
use sp_core::ecdsa;
use sp_io::{crypto::secp256k1_ecdsa_recover, hashing::keccak_256};
use sp_runtime::{
	transaction_validity::{
		InvalidTransaction, TransactionPriority, TransactionSource, TransactionValidity, ValidTransaction,
	},
	DispatchResult,
};
use sp_std::vec::Vec;

mod mock;
mod tests;

type EcdsaSignature = ecdsa::Signature;
type DestAddress = Vec<u8>;

pub trait Config: system::Config {
	type Event: From<Event<Self>> + Into<<Self as system::Config>::Event>;
	type Currency: BasicCurrency<Self::AccountId, Balance = Balance>;
	/// The RenVM split public key
	type PublicKey: Get<[u8; 20]>;
	/// The RenVM Currency identifier
	type CurrencyIdentifier: Get<[u8; 32]>;
	/// A configuration for base priority of unsigned transactions.
	///
	/// This is exposed so that it can be tuned for particular runtime, when
	/// multiple modules send unsigned transactions.
	type UnsignedPriority: Get<TransactionPriority>;
}

decl_storage! {
	trait Store for Module<T: Config> as Template {
		/// Signature blacklist. This is required to prevent double claim.
		Signatures get(fn signatures): map hasher(opaque_twox_256) EcdsaSignature => Option<()>;

		/// Record burn event details
		BurnEvents get(fn burn_events): map hasher(twox_64_concat) u32 => Option<(T::BlockNumber, DestAddress, Balance)>;
		/// Next burn event ID
		NextBurnEventId get(fn next_burn_event_id): u32;
	}
}

decl_event!(
	pub enum Event<T> where
		<T as system::Config>::AccountId,
	{
		/// Asset minted. \[owner, amount\]
		Minted(AccountId, Balance),
		/// Asset burnt in this chain \[owner, dest, amount\]
		Burnt(AccountId, DestAddress, Balance),
	}
);

decl_error! {
	pub enum Error for Module<T: Config> {
		/// The mint signature is invalid.
		InvalidMintSignature,
		/// The mint signature has already been used.
		SignatureAlreadyUsed,
		/// Burn ID overflow.
		BurnIdOverflow,
	}
}

decl_module! {
	pub struct Module<T: Config> for enum Call where origin: T::Origin {
		type Error = Error<T>;

		fn deposit_event() = default;

		/// Allow a user to mint if they have a valid signature from RenVM.
		///
		/// The dispatch origin of this call must be _None_.
		///
		/// Verify input by `validate_unsigned`
		#[weight = 10_000]
		fn mint(
			origin,
			who: T::AccountId,
			p_hash: [u8; 32],
			#[compact] amount: Balance,
			n_hash: [u8; 32],
			sig: EcdsaSignature,
		) {
			ensure_none(origin)?;
			Self::do_mint(who, amount, sig)?;
		}

		/// Allow a user to burn assets.
		#[weight = 10_000]
		fn burn(
			origin,
			to: DestAddress,
			#[compact] amount: Balance,
		) {
			let sender = ensure_signed(origin)?;

			NextBurnEventId::try_mutate(|id| -> DispatchResult {
				let this_id = *id;
				*id = id.checked_add(1).ok_or(Error::<T>::BurnIdOverflow)?;

				T::Currency::withdraw(&sender, amount)?;
				BurnEvents::<T>::insert(this_id, (frame_system::Module::<T>::block_number(), &to, amount));
				Self::deposit_event(RawEvent::Burnt(sender, to, amount));

				Ok(())
			})?;
		}
	}
}

impl<T: Config> Module<T> {
	fn do_mint(sender: T::AccountId, amount: Balance, sig: EcdsaSignature) -> DispatchResult {
		T::Currency::deposit(&sender, amount)?;
		Signatures::insert(&sig, ());

		Self::deposit_event(RawEvent::Minted(sender, amount));
		Ok(())
	}

	// ABI-encode the values for creating the signature hash.
	fn signable_message(p_hash: &[u8; 32], amount: u128, to: &[u8], n_hash: &[u8; 32], token: &[u8; 32]) -> Vec<u8> {
		// p_hash ++ amount ++ token ++ to ++ n_hash
		let length = 32 + 32 + 32 + 32 + 32;
		let mut v = Vec::with_capacity(length);
		v.extend_from_slice(&p_hash[..]);
		v.extend_from_slice(&[0u8; 16][..]);
		v.extend_from_slice(&amount.to_be_bytes()[..]);
		v.extend_from_slice(&token[..]);
		v.extend_from_slice(to);
		v.extend_from_slice(&n_hash[..]);
		v
	}

	// Verify that the signature has been signed by RenVM.
	fn verify_signature(
		p_hash: &[u8; 32],
		amount: u128,
		to: &[u8],
		n_hash: &[u8; 32],
		sig: &[u8; 65],
	) -> DispatchResult {
		let ren_btc_identifier = T::CurrencyIdentifier::get();

		let signed_message_hash = keccak_256(&Self::signable_message(p_hash, amount, to, n_hash, &ren_btc_identifier));
		let recoverd =
			secp256k1_ecdsa_recover(&sig, &signed_message_hash).map_err(|_| Error::<T>::InvalidMintSignature)?;
		let addr = &keccak_256(&recoverd)[12..];

		ensure!(addr == T::PublicKey::get(), Error::<T>::InvalidMintSignature);

		Ok(())
	}
}

#[allow(deprecated)]
impl<T: Config> frame_support::unsigned::ValidateUnsigned for Module<T> {
	type Call = Call<T>;

	fn validate_unsigned(_source: TransactionSource, call: &Self::Call) -> TransactionValidity {
		if let Call::mint(who, p_hash, amount, n_hash, sig) = call {
			// check if already exists
			if Signatures::contains_key(&sig) {
				return InvalidTransaction::Stale.into();
			}

			let verify_result = Encode::using_encoded(&who, |encoded| -> DispatchResult {
				Self::verify_signature(&p_hash, *amount, encoded, &n_hash, &sig.0)
			});

			// verify signature
			if verify_result.is_err() {
				return InvalidTransaction::BadProof.into();
			}

			ValidTransaction::with_tag_prefix("renvm-bridge")
				.priority(T::UnsignedPriority::get())
				.and_provides(sig)
				.longevity(64_u64)
				.propagate(true)
				.build()
		} else {
			InvalidTransaction::Call.into()
		}
	}
}
