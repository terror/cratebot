use {
  anyhow::bail,
  crates_io_api::{AsyncClient, Crate, CratesQuery},
  egg_mode::{self, auth, tweet::DraftTweet, KeyPair, Response, Token},
  rand::Rng,
  serde::Deserialize,
  sqlite::{Connection, State},
  std::{path::PathBuf, process, time::Duration},
};

const AGENT: &str = "cratebot";
const DB_PATH: &str = "db.sqlite";

#[derive(Debug, Deserialize)]
struct Config {
  pub(crate) access_token_key: String,
  pub(crate) access_token_secret: String,
  pub(crate) consumer_key: String,
  pub(crate) consumer_secret: String,
}

impl Config {
  fn from_env() -> Result<Self> {
    Ok(envy::from_env::<Self>()?)
  }
}

#[derive(Debug)]
pub struct Client {
  token: egg_mode::Token,
}

impl Client {
  async fn new(config: Config) -> Self {
    Client {
      token: Token::Access {
        consumer: KeyPair::new(config.consumer_key, config.consumer_secret),
        access: KeyPair::new(
          config.access_token_key,
          config.access_token_secret,
        ),
      },
    }
  }

  async fn tweet(&self, random_crate: Crate) -> Result {
    log::info!("Publishing tweet for crate {:?}", random_crate);
    Ok(())
  }
}

struct Api {
  client: AsyncClient,
}

impl Api {
  fn new(agent: &str, rate_limit: Duration) -> Result<Self> {
    Ok(Self {
      client: AsyncClient::new(agent, rate_limit)?,
    })
  }

  async fn get_crate(&self, crate_name: &str) -> Result<Crate> {
    Ok(self.client.get_crate(crate_name).await?.crate_data)
  }

  async fn crates(&self, starting_page: Option<u64>) -> Result<Vec<Crate>> {
    let mut page = starting_page.unwrap_or(1);

    let mut crates = Vec::new();

    loop {
      log::info!("Fetching crates from page {page}");

      let mut query = CratesQuery::builder().page_size(100).build();

      query.set_page(page);

      let response = self.client.crates(query).await?;

      if response.crates.is_empty() {
        break;
      }

      log::trace!("Fetched crates: {:?}", &response.crates);

      crates.extend(response.crates);

      page += 1;
    }

    Ok(crates)
  }
}

struct Db {
  conn: Connection,
}

impl Db {
  fn open(path: Option<PathBuf>) -> Result<Self> {
    Ok(Self {
      conn: sqlite::open(path.unwrap_or(PathBuf::from(":memory:")))?,
    })
  }

  fn table(&self, name: &str, columns: &[(&str, &str)]) -> Result {
    log::info!("Creating table {name} with columns {:?}", columns);

    Ok(self.conn.execute(format!(
        "CREATE TABLE IF NOT EXISTS {} ({})",
        name,
        columns
          .iter()
          .map(|(column, data_type)| format!("{column} {data_type}"))
          .collect::<Vec<String>>()
          .join(", ")
      ))?)
  }

  fn count(&self, name: &str) -> Result<i64> {
    log::info!("Fetching row count for table {name}");

    let mut statement =
      self.conn.prepare(format!("SELECT COUNT(*) FROM {name}"))?;

    if let State::Row = statement.next()? {
      return Ok(statement.read::<i64>(0)?);
    }

    bail!("Failed reading COUNT(*) for table {name}")
  }

  fn crates(&self) -> Result<Vec<String>> {
    log::info!("Fetching all crate names from db...");

    let mut statement = self.conn.prepare(format!("SELECT * FROM crates"))?;

    let mut ret = Vec::new();

    if let State::Row = statement.next()? {
      ret.push(statement.read::<String>(0)?);
    }

    Ok(ret)
  }

  fn sync(&self, crates: Vec<Crate>) -> Result {
    log::info!("Syncing db...");

    let crate_names = crates
      .iter()
      .map(|c| c.name.clone())
      .collect::<Vec<String>>();

    // Make sure its not in the database
    // Do one big insert
    // Use current timestamp for date field

    Ok(())
  }
}

type Result<T = (), E = anyhow::Error> = std::result::Result<T, E>;

async fn run() -> Result {
  let api = Api::new(AGENT, Duration::from_secs(1))?;

  let db = Db::open(Some(PathBuf::from(DB_PATH)))?;

  db.table(
    "crates",
    &[("name", "TEXT"), ("visited", "INTEGER"), ("date", "TEXT")],
  )?;

  db.sync(api.crates(Some(db.count("crates")?.try_into()?)).await?)?;

  let client = Client::new(Config::from_env()?).await;

  let crates = db.crates()?;

  client
    .tweet(
      api
        .get_crate(&crates[rand::thread_rng().gen_range(0..crates.len())])
        .await?,
    )
    .await?;

  Ok(())
}

#[tokio::main]
async fn main() {
  env_logger::init();

  if let Err(error) = run().await {
    println!("error: {error}");
    process::exit(1);
  }
}
