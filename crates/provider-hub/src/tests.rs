use super::*;
use client_provider::{AccountSettings, ConnectionState, HistoryImport, LinkStep, ProviderEvent};
use futures::stream::BoxStream;
use std::sync::atomic::AtomicUsize;
use tokio::sync::mpsc;

/// A provider of a few numbers that answers what it is told to.
struct Fake {
    id: &'static str,
    accounts: Mutex<Vec<Account>>,
    capabilities: Capabilities,
    places: Vec<LinkPlace>,
    /// What every call answers instead of its own answer, while set.
    failing: Mutex<Option<ProviderError>>,
    /// The chats cannot be listed, and nothing else is wrong.
    chats_down: AtomicBool,
    /// The senders of the streams it handed out.
    streams: Mutex<Vec<mpsc::UnboundedSender<ProviderEvent>>>,
    subscribed: AtomicUsize,
    created: AtomicUsize,
}

fn account(id: &str) -> Account {
    Account {
        id: AccountId::new(id),
        display_name: id.to_owned(),
        phone: None,
        self_contact: None,
        connection: ConnectionState::Connected,
        settings: AccountSettings::default(),
    }
}

fn fake(id: &'static str, accounts: &[&str], capabilities: Capabilities) -> Arc<Fake> {
    Arc::new(Fake {
        id,
        accounts: Mutex::new(accounts.iter().map(|id| account(id)).collect()),
        capabilities,
        places: Vec::new(),
        failing: Mutex::new(None),
        chats_down: AtomicBool::new(false),
        streams: Mutex::new(Vec::new()),
        subscribed: AtomicUsize::new(0),
        created: AtomicUsize::new(0),
    })
}

impl Fake {
    fn fail(&self, error: Option<ProviderError>) {
        *self.failing.lock().unwrap() = error;
    }

    fn weather(&self) -> ProviderResult<()> {
        match self.failing.lock().unwrap().clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn say(&self, event: ProviderEvent) {
        for stream in self.streams.lock().unwrap().iter() {
            let _ = stream.send(event.clone());
        }
    }

    /// Ends every stream it handed out.
    fn hang_up(&self) {
        self.streams.lock().unwrap().clear();
    }
}

#[async_trait]
impl Provider for Fake {
    fn id(&self) -> &'static str {
        self.id
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
        self.weather()?;
        Ok(self.accounts.lock().unwrap().clone())
    }

    async fn list_chats(&self, _: &AccountId, _: Option<Cursor>) -> ProviderResult<Page<Chat>> {
        self.weather()?;
        if self.chats_down.load(Ordering::SeqCst) {
            return Err(ProviderError::Transient(
                "the chats are not available".into(),
            ));
        }
        Ok(Page {
            items: Vec::new(),
            next_cursor: Some(Cursor::new(self.id)),
        })
    }

    async fn fetch_messages(
        &self,
        _: &AccountId,
        _: &ChatId,
        _: Option<Cursor>,
        _: u32,
    ) -> ProviderResult<Page<Message>> {
        Err(ProviderError::Unsupported("messages"))
    }

    async fn send(&self, _: OutgoingMessage) -> ProviderResult<SendReceipt> {
        Err(ProviderError::Unsupported("sending"))
    }

    async fn mark_read(
        &self,
        _: &AccountId,
        _: &ChatId,
        _: Option<&MessageId>,
    ) -> ProviderResult<()> {
        self.weather()
    }

    async fn download_media(&self, _: &AccountId, _: &MediaRef) -> ProviderResult<MediaData> {
        Err(ProviderError::Unsupported("files"))
    }

    async fn link_places(&self) -> ProviderResult<Vec<LinkPlace>> {
        self.weather()?;
        Ok(self.places.clone())
    }

    async fn create_account(&self, new: &NewAccount) -> ProviderResult<LinkStatus> {
        self.weather()?;
        let number = self.created.fetch_add(1, Ordering::SeqCst);
        let account = account(&format!("{}-new-{number}", self.id));
        self.accounts.lock().unwrap().push(account.clone());
        let _ = new;
        Ok(LinkStatus {
            account,
            step: LinkStep::Starting,
            was_linked: false,
        })
    }

    async fn delete_account(&self, account: &AccountId) -> ProviderResult<()> {
        self.weather()?;
        self.accounts
            .lock()
            .unwrap()
            .retain(|known| &known.id != account);
        Ok(())
    }

    async fn subscribe(&self) -> ProviderResult<EventStream> {
        self.weather()?;
        self.subscribed.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = mpsc::unbounded_channel();
        self.streams.lock().unwrap().push(sender);
        let events: BoxStream<'static, ProviderEvent> =
            futures::stream::unfold(receiver, |mut receiver| async move {
                receiver.recv().await.map(|event| (event, receiver))
            })
            .boxed();
        Ok(events)
    }
}

fn place(city: &str) -> LinkPlace {
    LinkPlace {
        country: "VE".into(),
        country_name: "Venezuela".into(),
        city: city.into(),
        city_name: city.into(),
    }
}

fn new_account(request: &str, place: Option<LinkPlace>) -> NewAccount {
    NewAccount {
        name: None,
        place,
        pairing_phone: None,
        history: HistoryImport::Recent,
        request_id: request.into(),
    }
}

fn ids(accounts: &[Account]) -> Vec<&str> {
    accounts.iter().map(|account| account.id.as_str()).collect()
}

fn connected(account: &str) -> ProviderEvent {
    ProviderEvent::ConnectionChanged {
        account_id: AccountId::new(account),
        state: ConnectionState::Connected,
    }
}

/// A hub of a provider on this computer and one in the cloud.
fn pair() -> (HubProvider, Arc<Fake>, Arc<Fake>) {
    let local = fake(
        "local",
        &["l1"],
        Capabilities {
            calls: true,
            link_accounts: true,
            ..Capabilities::none()
        },
    );
    let cloud = fake(
        "cloud",
        &["c1", "c2"],
        Capabilities {
            sticker_favorites: true,
            link_accounts: true,
            ..Capabilities::none()
        },
    );
    let hub = HubProvider::new(vec![local.clone(), cloud.clone()]);
    (hub, local, cloud)
}

#[tokio::test]
async fn the_numbers_of_every_member_are_listed_and_each_call_goes_to_its_own() {
    let (hub, _, _) = pair();
    assert_eq!(ids(&hub.list_accounts().await.unwrap()), ["l1", "c1", "c2"]);
    assert_eq!(hub.owner_of(&AccountId::new("l1")), Some("local"));
    assert_eq!(hub.owner_of(&AccountId::new("c2")), Some("cloud"));

    // Each fake signs its chat pages with its own id.
    let page = hub.list_chats(&AccountId::new("c1"), None).await.unwrap();
    assert_eq!(page.next_cursor.unwrap().as_str(), "cloud");
    let page = hub.list_chats(&AccountId::new("l1"), None).await.unwrap();
    assert_eq!(page.next_cursor.unwrap().as_str(), "local");

    // A number nobody has is turned down, not guessed.
    let nobody = hub.list_chats(&AccountId::new("x"), None).await;
    assert!(
        matches!(nobody, Err(ProviderError::Rejected { code, .. }) if code == "unknown_account")
    );
}

#[tokio::test]
async fn what_one_number_can_do_is_what_its_member_can() {
    let (hub, _, _) = pair();
    hub.list_accounts().await.unwrap();
    // Everything anybody can, for the places that ask about no number.
    assert!(hub.capabilities().calls && hub.capabilities().sticker_favorites);
    let local = hub.capabilities_for(&AccountId::new("l1"));
    assert!(local.calls && !local.sticker_favorites);
    let cloud = hub.capabilities_for(&AccountId::new("c1"));
    assert!(!cloud.calls && cloud.sticker_favorites);
}

#[tokio::test]
async fn a_member_that_refuses_its_credentials_is_set_aside_and_the_others_go_on() {
    let (hub, _, cloud) = pair();
    hub.list_accounts().await.unwrap();
    let refusals = hub.refusals();
    assert_eq!(*refusals.borrow(), None);

    cloud.fail(Some(ProviderError::Unauthorized("key revoked".into())));
    let answer = hub
        .mark_read(&AccountId::new("c1"), &ChatId::new("a"), None)
        .await;
    // Never `Unauthorized`: that would stop the engine for everybody.
    assert!(matches!(answer, Err(ProviderError::Rejected { code, .. }) if code == SIGNED_OUT));
    assert_eq!(
        *refusals.borrow(),
        Some(Refusal {
            provider: "cloud",
            reason: "key revoked".into()
        })
    );

    // Its numbers stay listed, the other member works, and it is not
    // asked again.
    cloud.fail(None);
    assert_eq!(ids(&hub.list_accounts().await.unwrap()), ["l1", "c1", "c2"]);
    hub.mark_read(&AccountId::new("l1"), &ChatId::new("a"), None)
        .await
        .unwrap();
    let again = hub
        .mark_read(&AccountId::new("c1"), &ChatId::new("a"), None)
        .await;
    assert!(matches!(again, Err(ProviderError::Rejected { code, .. }) if code == SIGNED_OUT));

    // Signed in again: a new member of the same id takes its place.
    hub.attach(
        fake("cloud", &["c1"], Capabilities::none()),
        Some(vec![account("c1"), account("c2")]),
    );
    assert_eq!(*refusals.borrow(), None);
    assert_eq!(ids(&hub.list_accounts().await.unwrap()), ["l1", "c1"]);
    hub.mark_read(&AccountId::new("c1"), &ChatId::new("a"), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_member_that_cannot_be_reached_keeps_the_numbers_it_had() {
    let local = fake("local", &["l1"], Capabilities::none());
    let cloud = fake("cloud", &["c1"], Capabilities::none());
    cloud.fail(Some(ProviderError::Transient("offline".into())));

    // Nothing known of its numbers: the list waits rather than say none.
    let hub = HubProvider::new(vec![local.clone(), cloud.clone()]);
    assert!(hub.list_accounts().await.is_err());

    // Known from the last run: they stand in, and the other member lists.
    let hub = HubProvider::new(vec![local.clone()]);
    hub.attach(cloud.clone(), Some(vec![account("c1")]));
    assert_eq!(ids(&hub.list_accounts().await.unwrap()), ["l1", "c1"]);
    assert_eq!(hub.owner_of(&AccountId::new("c1")), Some("cloud"));

    // Listed once, then weather: what it said last stands.
    cloud.fail(None);
    cloud.accounts.lock().unwrap().push(account("c2"));
    assert_eq!(ids(&hub.list_accounts().await.unwrap()), ["l1", "c1", "c2"]);
    cloud.fail(Some(ProviderError::Transient("offline".into())));
    assert_eq!(ids(&hub.list_accounts().await.unwrap()), ["l1", "c1", "c2"]);
}

#[tokio::test]
async fn a_member_that_leaves_takes_only_its_numbers() {
    let (hub, _, _) = pair();
    hub.list_accounts().await.unwrap();
    hub.detach("cloud");
    assert!(!hub.has("cloud") && hub.has("local"));
    assert_eq!(ids(&hub.list_accounts().await.unwrap()), ["l1"]);
    assert_eq!(hub.owner_of(&AccountId::new("c1")), None);
    assert!(!hub.capabilities().sticker_favorites);
}

#[tokio::test]
async fn a_number_is_linked_where_its_place_is_or_where_none_is_asked() {
    let local = fake(
        "local",
        &[],
        Capabilities {
            link_accounts: true,
            ..Capabilities::none()
        },
    );
    let cloud = Arc::new(Fake {
        places: vec![place("ccs")],
        ..Arc::try_unwrap(fake(
            "cloud",
            &[],
            Capabilities {
                link_accounts: true,
                ..Capabilities::none()
            },
        ))
        .ok()
        .unwrap()
    });
    // The cloud first: order does not send a number with no place to it
    // once it is known to ask for one.
    let hub = HubProvider::new(vec![cloud.clone(), local.clone()]);
    assert_eq!(hub.link_places().await.unwrap(), [place("ccs")]);
    assert_eq!(hub.owner_of_place(&place("ccs")), Some("cloud"));

    let here = hub.create_account(&new_account("r1", None)).await.unwrap();
    assert_eq!(hub.owner_of(&here.account.id), Some("local"));
    let there = hub
        .create_account(&new_account("r2", Some(place("ccs"))))
        .await
        .unwrap();
    assert_eq!(hub.owner_of(&there.account.id), Some("cloud"));
    // The new numbers are listed before their members are asked again.
    cloud.fail(Some(ProviderError::Transient("offline".into())));
    let listed = hub.list_accounts().await.unwrap();
    assert!(ids(&listed).contains(&there.account.id.as_str()));
    cloud.fail(None);

    // A retry of the same attempt goes to the same member.
    hub.create_account(&new_account("r2", None)).await.unwrap();
    assert_eq!(cloud.created.load(Ordering::SeqCst), 2);
    assert_eq!(local.created.load(Ordering::SeqCst), 1);

    // A place nobody listed is not a place.
    let lost = hub
        .create_account(&new_account("r3", Some(place("mar"))))
        .await;
    assert!(matches!(lost, Err(ProviderError::Rejected { code, .. }) if code == "no_place"));

    // A deleted number is forgotten.
    hub.delete_account(&here.account.id).await.unwrap();
    assert_eq!(hub.owner_of(&here.account.id), None);
}

#[tokio::test]
async fn the_live_updates_of_all_come_together_and_end_when_one_ends() {
    let (hub, local, cloud) = pair();
    let mut events = hub.subscribe().await.unwrap();
    local.say(connected("l1"));
    cloud.say(connected("c1"));
    let mut heard = vec![events.next().await.unwrap(), events.next().await.unwrap()];
    heard.sort_by_key(|event| format!("{event:?}"));
    assert_eq!(heard, [connected("c1"), connected("l1")]);

    // One member's stream ends: the whole ends, so that the engine comes
    // back and refreshes.
    cloud.hang_up();
    assert_eq!(events.next().await, None);
}

#[tokio::test]
async fn a_change_of_membership_cuts_the_live_updates() {
    let (hub, local, _) = pair();
    let mut events = hub.subscribe().await.unwrap();
    hub.detach("cloud");
    assert_eq!(events.next().await, None);

    // Whoever comes back hears the members there are now.
    let mut events = hub.subscribe().await.unwrap();
    local.say(connected("l1"));
    assert_eq!(events.next().await, Some(connected("l1")));
    hub.attach(fake("cloud", &[], Capabilities::none()), None);
    assert_eq!(events.next().await, None);
}

#[tokio::test(start_paused = true)]
async fn a_member_whose_updates_cannot_be_opened_does_not_hold_the_others_back() {
    let (hub, local, cloud) = pair();
    cloud.fail(Some(ProviderError::Transient("offline".into())));
    let mut events = hub.subscribe().await.unwrap();
    local.say(connected("l1"));
    assert_eq!(events.next().await, Some(connected("l1")));
    // And the stream ends by itself a little later, to try it again.
    cloud.fail(None);
    assert_eq!(events.next().await, None);
    let before = cloud.subscribed.load(Ordering::SeqCst);
    let _events = hub.subscribe().await.unwrap();
    assert_eq!(cloud.subscribed.load(Ordering::SeqCst), before + 1);
}

#[tokio::test]
async fn a_hub_of_nobody_is_quiet() {
    let hub = HubProvider::new(Vec::new());
    assert!(hub.list_accounts().await.unwrap().is_empty());
    assert_eq!(hub.capabilities(), Capabilities::none());
    let mut events = hub.subscribe().await.unwrap();
    let nothing = tokio::time::timeout(Duration::from_millis(20), events.next()).await;
    assert!(nothing.is_err(), "no end and no event");
}

/// The engine on a hub of two providers: the numbers of both are in the
/// store, each under its own provider, and the ones of a member that
/// leaves are forgotten with their chats while the others stay.
#[test]
fn the_engine_keeps_the_numbers_of_every_member_and_forgets_those_of_one_that_leaves() {
    use client_core::{Store, SyncConfig, SyncEngine};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let hub = HubProvider::new(vec![
        Arc::new(provider_mock::MockProvider::quiet()),
        Arc::new(provider_example::EchoProvider::default()),
    ]);
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = SyncEngine::new(
        store.clone(),
        Arc::new(hub.clone()),
        SyncConfig::default(),
        runtime.handle().clone(),
    );
    runtime.block_on(engine.refresh()).unwrap();

    let of = |provider: &str| store.accounts_of(provider).unwrap();
    let (mock, example) = (of("mock"), of("example"));
    assert_eq!((mock.len(), example.len()), (2, 1));
    assert_eq!(store.accounts().unwrap().len(), 3);
    let echo = example[0].id.clone();
    assert!(!store.chats(&echo, None).unwrap().is_empty());
    assert!(!store.chats(&mock[0].id, None).unwrap().is_empty());
    // Each number can do what its own provider can.
    assert_eq!(
        engine.capabilities_of(&echo),
        provider_example::EchoProvider::default().capabilities()
    );
    assert_ne!(
        engine.capabilities_of(&echo),
        engine.capabilities_of(&mock[0].id)
    );

    hub.detach("example");
    runtime.block_on(engine.refresh()).unwrap();
    assert!(of("example").is_empty());
    assert!(store.chats(&echo, None).unwrap().is_empty());
    assert_eq!(of("mock").len(), 2);
    assert!(!store.chats(&mock[0].id, None).unwrap().is_empty());
}

/// A member whose chats cannot be listed does not keep the others from
/// being refreshed: the refresh fails only when nobody answered.
#[test]
fn the_engine_refreshes_the_members_that_answer_when_one_does_not() {
    use client_core::{Store, SyncConfig, SyncEngine};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let cloud = fake("cloud", &["c1"], Capabilities::none());
    cloud.chats_down.store(true, Ordering::SeqCst);
    let engine_on = |members: Vec<Arc<dyn Provider>>| {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let engine = SyncEngine::new(
            store.clone(),
            Arc::new(HubProvider::new(members)),
            SyncConfig::default(),
            runtime.handle().clone(),
        );
        (engine, store)
    };

    let (engine, store) = engine_on(vec![
        Arc::new(provider_mock::MockProvider::quiet()),
        cloud.clone(),
    ]);
    runtime
        .block_on(engine.refresh())
        .expect("the member that answers is refreshed");
    let mock = store.accounts_of("mock").unwrap();
    assert!(!store.chats(&mock[0].id, None).unwrap().is_empty());
    // The number of the one that did not is kept, with nothing new.
    assert_eq!(store.accounts_of("cloud").unwrap().len(), 1);

    // Alone, its failure is the refresh's, as with any single provider.
    let (engine, _store) = engine_on(vec![cloud.clone()]);
    assert!(runtime.block_on(engine.refresh()).is_err());
}
