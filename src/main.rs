use {
  anyhow::{anyhow, bail},
  chrono::Utc,
  crates_io_api::{AsyncClient, Crate, CratesQuery},
  dotenv::dotenv,
  egg_mode::{self, auth, tweet::DraftTweet, KeyPair, Response, Token},
  rand::{seq::SliceRandom, Rng},
  serde::Deserialize,
  sqlite::{Connection, State, Value},
  std::{path::PathBuf, process, time::Duration},
};

const AGENT: &str = "cratebot";
const DB_PATH: &str = "db.sqlite";
const PAGE_SIZE: u64 = 100;

#[derive(Debug, Deserialize)]
struct Config {
  pub(crate) access_token_key: String,
  pub(crate) access_token_secret: String,
  pub(crate) consumer_key: String,
  pub(crate) consumer_secret: String,
}

impl Config {
  fn from_env() -> Result<Self> {
    dotenv().ok();
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

  async fn tweet(&self, random_crate: Crate) -> Result<Crate> {
    log::info!("Publishing tweet for crate {:?}", random_crate);

    let Crate {
      name, description, ..
    } = random_crate.clone();

    DraftTweet::new(format!(
      "{}\n{}\n{}",
      name,
      description.unwrap_or("".into()),
      format!("https://crates.io/crates/{}", name)
    ))
    .send(&self.token)
    .await?;

    Ok(random_crate)
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
      log::info!("Fetching crates from page {page}...");

      let mut query = CratesQuery::builder().page_size(PAGE_SIZE).build();

      query.set_page(page);

      let response = self.client.crates(query).await?;

      if response.crates.is_empty() {
        break;
      }

      log::trace!(
        "Fetched crates: {:?}",
        response
          .crates
          .iter()
          .map(|c| c.name.clone())
          .collect::<Vec<String>>()
      );

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

    let mut statement = self
      .conn
      .prepare("SELECT * FROM crates WHERE visited = 0")?;

    let mut ret = Vec::new();

    if let State::Row = statement.next()? {
      ret.push(statement.read::<String>(0)?);
    }

    Ok(ret)
  }

  fn update(&self, name: &str) -> Result {
    Ok(self.conn.execute(format!(
      "UPDATE crates SET visited = 1 where name = '{name}'"
    ))?)
  }

  fn sync(&self, crates: Vec<Crate>) -> Result {
    log::info!("Syncing db...");

    let names = crates
      .iter()
      .map(|c| c.name.clone())
      .collect::<Vec<String>>();

    let mut query = String::new();

    for name in names {
      if let State::Done = self
        .conn
        .prepare("SELECT * FROM crates WHERE name = :name")?
        .bind_by_name(":name", name.as_str())?
        .next()?
      {
        query.push_str(&format!(
          "INSERT INTO crates (name, visited, date) VALUES ('{}', {}, '{}');\n",
          name,
          0,
          Utc::now().timestamp()
        ));
      }
    }

    if query.is_empty() {
      log::info!("Database up to date!");
      return Ok(());
    }

    log::info!("Executing query {query}");
    self.conn.execute(query.clone())?;

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

  db.sync(
    api
      .crates(Some(
        (db.count("crates")? / PAGE_SIZE as i64 + 1).try_into()?,
      ))
      .await?,
  )?;

  db.update(
    &Client::new(Config::from_env()?)
      .await
      .tweet(
        api
          .get_crate(
            &db
              .crates()?
              .choose(&mut rand::thread_rng())
              .ok_or_else(|| {
                anyhow!(
                  "Failed to choose a random crate from crates in the database"
                )
              })?
              .to_string(),
          )
          .await?,
      )
      .await?
      .name,
  )?;

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
