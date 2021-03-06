#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::unnecessary_cast)]

use codec::{Decode, Encode};
use enumflags2::BitFlags;
use frame_support::{
	decl_error, decl_event, decl_module, ensure,
	traits::{Currency, ExistenceRequirement::KeepAlive, Get},
	transactional,
	weights::Weight,
	IterableStorageDoubleMap,
};
use frame_system::ensure_signed;
use orml_traits::{BasicCurrency, BasicReservableCurrency, NFT};
use primitives::{Balance, NFTBalance};
use sp_runtime::{
	traits::{AccountIdConversion, Zero},
	DispatchResult, ModuleId, RuntimeDebug,
};

mod default_weight;
mod mock;
mod tests;

pub trait WeightInfo {
	fn create_class() -> Weight;
	fn mint(i: u32) -> Weight;
	fn transfer() -> Weight;
	fn burn() -> Weight;
	fn destroy_class() -> Weight;
}

pub type CID = sp_std::vec::Vec<u8>;

#[repr(u8)]
#[derive(Encode, Decode, Clone, Copy, BitFlags, RuntimeDebug, PartialEq, Eq)]
pub enum ClassProperty {
	/// Token can be transferred
	Transferable = 0b00000001,
	/// Token can be burned
	Burnable = 0b00000010,
}

#[derive(Clone, Copy, PartialEq, Default, RuntimeDebug)]
pub struct Properties(pub BitFlags<ClassProperty>);

impl Eq for Properties {}
impl Encode for Properties {
	fn using_encoded<R, F: FnOnce(&[u8]) -> R>(&self, f: F) -> R {
		self.0.bits().using_encoded(f)
	}
}
impl Decode for Properties {
	fn decode<I: codec::Input>(input: &mut I) -> sp_std::result::Result<Self, codec::Error> {
		let field = u8::decode(input)?;
		Ok(Self(
			<BitFlags<ClassProperty>>::from_bits(field as u8).map_err(|_| "invalid value")?,
		))
	}
}

#[derive(Encode, Decode, Clone, RuntimeDebug, PartialEq, Eq)]
pub struct ClassData {
	/// The minimum balance to create class
	pub deposit: Balance,
	/// Property of token
	pub properties: Properties,
}

#[derive(Encode, Decode, Clone, RuntimeDebug, PartialEq, Eq)]
pub struct TokenData {
	/// The minimum balance to create token
	pub deposit: Balance,
}

pub type TokenIdOf<T> = <T as orml_nft::Config>::TokenId;
pub type ClassIdOf<T> = <T as orml_nft::Config>::ClassId;

decl_event!(
	 pub enum Event<T> where
		<T as frame_system::Config>::AccountId,
		ClassId = ClassIdOf<T>,
		TokenId = TokenIdOf<T>,
	{
		 /// Created NFT class. \[owner, class_id\]
		 CreatedClass(AccountId, ClassId),
		 /// Minted NFT token. \[from, to, class_id, quantity\]
		 MintedToken(AccountId, AccountId, ClassId, u32),
		 /// Transfered NFT token. \[from, to, class_id, token_id\]
		 TransferedToken(AccountId, AccountId, ClassId, TokenId),
		 /// Burned NFT token. \[owner, class_id, token_id\]
		 BurnedToken(AccountId, ClassId, TokenId),
		 /// Destroyed NFT class. \[owner, class_id, dest\]
		 DestroyedClass(AccountId, ClassId, AccountId),
	}
);

decl_error! {
	/// Error for module-nft module.
	pub enum Error for Module<T: Config> {
		/// ClassId not found
		ClassIdNotFound,
		/// TokenId not found
		TokenIdNotFound,
		/// The operator is not the owner of the token and has no permission
		NoPermission,
		/// Quantity is invalid. need >= 1
		InvalidQuantity,
		/// Property of class don't support transfer
		NonTransferable,
		/// Property of class don't support burn
		NonBurnable,
		/// Can not destroy class
		/// Total issuance is not 0
		CannotDestroyClass,
	}
}

pub trait Config:
	frame_system::Config + orml_nft::Config<ClassData = ClassData, TokenData = TokenData> + pallet_proxy::Config
{
	type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
	/// The minimum balance to create class
	type CreateClassDeposit: Get<Balance>;
	/// The minimum balance to create token
	type CreateTokenDeposit: Get<Balance>;
	/// The NFT's module id
	type ModuleId: Get<ModuleId>;
	///  Currency type for reserve/unreserve balance to
	/// create_class/mint/burn/destroy_class
	type Currency: BasicReservableCurrency<Self::AccountId, Balance = Balance>;
	/// Weight information for the extrinsics in this module.
	type WeightInfo: WeightInfo;
}

decl_module! {
	pub struct Module<T: Config> for enum Call where origin: T::Origin {
		type Error = Error<T>;
		fn deposit_event() = default;

		/// The minimum balance to create class
		const CreateClassDeposit: Balance = T::CreateClassDeposit::get();

		/// The minimum balance to create token
		const CreateTokenDeposit: Balance = T::CreateTokenDeposit::get();

		/// The NFT's module id
		const ModuleId: ModuleId = T::ModuleId::get();

		/// Create NFT class, tokens belong to the class.
		///
		/// - `metadata`: external metadata
		/// - `properties`: class property, include `Transferable` `Burnable`
		///
		/// # <weight>
		/// - Preconditions:
		/// 	- T::Currency is orml_currencies
		/// - Complexity: `O(1)`
		/// - Db reads: 3
		/// - Db writes: 4
		/// -------------------
		/// Base Weight:
		///		- best case: 231.1 µs
		///		- worst case: 233.7 µs
		/// # </weight>
		#[weight = <T as Config>::WeightInfo::create_class()]
		#[transactional]
		pub fn create_class(origin, metadata: CID, properties: Properties) {
			let who = ensure_signed(origin)?;
			let next_id = orml_nft::Module::<T>::next_class_id();
			let owner: T::AccountId = T::ModuleId::get().into_sub_account(next_id);
			let deposit = T::CreateClassDeposit::get();

			// it depends https://github.com/paritytech/substrate/issues/7563
			<T as Config>::Currency::transfer(&who, &owner, deposit)?;
			// Currently, use `free_balance(owner)` instead of `deposit`.
			<T as Config>::Currency::reserve(&owner, <T as Config>::Currency::free_balance(&owner))?;

			// owner add proxy delegate to origin
			let proxy_deposit = <pallet_proxy::Module<T>>::deposit(1u32);
			<T as pallet_proxy::Config>::Currency::transfer(&who, &owner, proxy_deposit, KeepAlive)?;
			<pallet_proxy::Module<T>>::add_proxy_delegate(&owner, who, Default::default(), Zero::zero())?;

			let data = ClassData { deposit, properties };
			orml_nft::Module::<T>::create_class(&owner, metadata, data)?;

			Self::deposit_event(RawEvent::CreatedClass(owner, next_id));
		}

		/// Mint NFT token
		///
		/// - `to`: the token owner's account
		/// - `class_id`: token belong to the class id
		/// - `metadata`: external metadata
		/// - `quantity`: token quantity
		///
		/// # <weight>
		/// - Preconditions:
		/// 	- T::Currency is orml_currencies
		/// - Complexity: `O(1)`
		/// - Db reads: 4
		/// - Db writes: 4
		/// -------------------
		/// Base Weight:
		///		- best case: 202 µs
		///		- worst case: 208 µs
		/// # </weight>
		#[weight = <T as Config>::WeightInfo::mint(*quantity)]
		#[transactional]
		pub fn mint(origin, to: T::AccountId, class_id: ClassIdOf<T>, metadata: CID, quantity: u32) {
			let who = ensure_signed(origin)?;
			ensure!(quantity >= 1, Error::<T>::InvalidQuantity);
			let class_info = orml_nft::Module::<T>::classes(class_id).ok_or(Error::<T>::ClassIdNotFound)?;
			ensure!(who == class_info.owner, Error::<T>::NoPermission);
			let deposit = T::CreateTokenDeposit::get();
			let total_deposit = deposit * (quantity as u128);
			<T as Config>::Currency::reserve(&class_info.owner, total_deposit)?;

			let data = TokenData { deposit };
			for _ in 0..quantity {
				orml_nft::Module::<T>::mint(&to, class_id, metadata.clone(), data.clone())?;
			}

			Self::deposit_event(RawEvent::MintedToken(who, to, class_id, quantity));
		}

		/// Transfer NFT token to another account
		///
		/// - `to`: the token owner's account
		/// - `token`: (class_id, token_id)
		///
		/// # <weight>
		/// - Preconditions:
		/// 	- T::Currency is orml_currencies
		/// - Complexity: `O(1)`
		/// - Db reads: 3
		/// - Db writes: 3
		/// -------------------
		/// Base Weight:
		///		- best case: 97.81 µs
		///		- worst case: 99.99 µs
		/// # </weight>
		#[weight = <T as Config>::WeightInfo::transfer()]
		pub fn transfer(origin, to: T::AccountId, token: (ClassIdOf<T>, TokenIdOf<T>)) {
			let who = ensure_signed(origin)?;
			Self::do_transfer(&who, &to, token)?;
		}

		/// Burn NFT token
		///
		/// - `token`: (class_id, token_id)
		///
		/// # <weight>
		/// - Preconditions:
		/// 	- T::Currency is orml_currencies
		/// - Complexity: `O(1)`
		/// - Db reads: 5
		/// - Db writes: 5
		/// -------------------
		/// Base Weight:
		///		- best case: 261.2 µs
		///		- worst case: 261.4 µs
		/// # </weight>
		#[weight = <T as Config>::WeightInfo::burn()]
		#[transactional]
		pub fn burn(origin, token: (ClassIdOf<T>, TokenIdOf<T>)) {
			let who = ensure_signed(origin)?;
			let class_info = orml_nft::Module::<T>::classes(token.0).ok_or(Error::<T>::ClassIdNotFound)?;
			let data = class_info.data;
			ensure!(data.properties.0.contains(ClassProperty::Burnable), Error::<T>::NonBurnable);

			let token_info = orml_nft::Module::<T>::tokens(token.0, token.1).ok_or(Error::<T>::TokenIdNotFound)?;
			ensure!(who == token_info.owner, Error::<T>::NoPermission);

			orml_nft::Module::<T>::burn(&who, token)?;
			let owner: T::AccountId = T::ModuleId::get().into_sub_account(token.0);
			let data = token_info.data;
			// `repatriate_reserved` will check `to` account exist and return `DeadAccount`.
			// `transfer` not do this check.
			<T as Config>::Currency::unreserve(&owner, data.deposit);
			<T as Config>::Currency::transfer(&owner, &who, data.deposit)?;

			Self::deposit_event(RawEvent::BurnedToken(who, token.0, token.1));
		}

		/// Destroy NFT class
		///
		/// - `class_id`: destroy class id
		/// - `dest`: transfer reserve balance from sub_account to dest
		///
		/// # <weight>
		/// - Preconditions:
		/// 	- T::Currency is orml_currencies
		/// - Complexity: `O(1)`
		/// - Db reads: 3
		/// - Db writes: 3
		/// -------------------
		/// Base Weight:
		///		- best case: 224.3 µs
		///		- worst case: 224.7 µs
		/// # </weight>
		#[weight = <T as Config>::WeightInfo::destroy_class()]
		#[transactional]
		pub fn destroy_class(origin, class_id: ClassIdOf<T>, dest: T::AccountId) {
			let who = ensure_signed(origin)?;
			let class_info = orml_nft::Module::<T>::classes(class_id).ok_or(Error::<T>::ClassIdNotFound)?;
			ensure!(who == class_info.owner, Error::<T>::NoPermission);
			ensure!(class_info.total_issuance == Zero::zero(), Error::<T>::CannotDestroyClass);

			let owner: T::AccountId = T::ModuleId::get().into_sub_account(class_id);
			let data = class_info.data;
			// `repatriate_reserved` will check `to` account exist and return `DeadAccount`.
			// `transfer` not do this check.
			<T as Config>::Currency::unreserve(&owner, data.deposit);
			<T as Config>::Currency::transfer(&owner, &dest, data.deposit)?;

			// transfer all free from origin to dest
			orml_nft::Module::<T>::destroy_class(&who, class_id)?;

			Self::deposit_event(RawEvent::DestroyedClass(who, class_id, dest));
		}
	}
}

impl<T: Config> Module<T> {
	/// Ensured atomic.
	#[transactional]
	fn do_transfer(from: &T::AccountId, to: &T::AccountId, token: (ClassIdOf<T>, TokenIdOf<T>)) -> DispatchResult {
		let class_info = orml_nft::Module::<T>::classes(token.0).ok_or(Error::<T>::ClassIdNotFound)?;
		let data = class_info.data;
		ensure!(
			data.properties.0.contains(ClassProperty::Transferable),
			Error::<T>::NonTransferable
		);

		let token_info = orml_nft::Module::<T>::tokens(token.0, token.1).ok_or(Error::<T>::TokenIdNotFound)?;
		ensure!(*from == token_info.owner, Error::<T>::NoPermission);

		orml_nft::Module::<T>::transfer(from, to, token)?;

		Self::deposit_event(RawEvent::TransferedToken(from.clone(), to.clone(), token.0, token.1));

		Ok(())
	}
}

impl<T: Config> NFT<T::AccountId> for Module<T> {
	type ClassId = ClassIdOf<T>;
	type TokenId = TokenIdOf<T>;
	type Balance = NFTBalance;

	fn balance(who: &T::AccountId) -> Self::Balance {
		orml_nft::TokensByOwner::<T>::iter_prefix(who).count() as u128
	}

	fn owner(token: (Self::ClassId, Self::TokenId)) -> Option<T::AccountId> {
		orml_nft::Module::<T>::tokens(token.0, token.1).map(|t| t.owner)
	}

	fn transfer(from: &T::AccountId, to: &T::AccountId, token: (Self::ClassId, Self::TokenId)) -> DispatchResult {
		Self::do_transfer(from, to, token)
	}
}
