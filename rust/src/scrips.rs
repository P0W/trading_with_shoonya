#![allow(dead_code)]
pub mod scrips {

    use std::fs;
    use std::fs::File;
    use std::io;
    use std::path::Path;
    use zip::read::ZipArchive;
    pub enum Exchange {
        NSE = 0,
        NFO = 1,
        CDS = 2,
        MCX = 3,
        BSE = 4,
        BFO = 5,
    }

    const DOWNLOAD_PATH: &str = "./downloads";

    pub fn download_scrip(exchange: &Exchange) {
        let url = match exchange {
            Exchange::NSE => "https://api.shoonya.com/NSE_symbols.txt.zip",
            Exchange::NFO => "https://api.shoonya.com/NFO_symbols.txt.zip",
            Exchange::CDS => "https://api.shoonya.com/CDS_symbols.txt.zip",
            Exchange::MCX => "https://api.shoonya.com/MCX_symbols.txt.zip",
            Exchange::BSE => "https://api.shoonya.com/BSE_symbols.txt.zip",
            Exchange::BFO => "https://api.shoonya.com/BFO_symbols.txt.zip",
        };

        // get today's date in YYYY-MM-DD format
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();

        // convert exchange to string
        let exchange = match exchange {
            Exchange::NSE => "NSE",
            Exchange::NFO => "NFO",
            Exchange::CDS => "CDS",
            Exchange::MCX => "MCX",
            Exchange::BSE => "BSE",
            Exchange::BFO => "BFO",
        };

        let download_file: String = format!("{}/{}_symbols_{}.txt", DOWNLOAD_PATH, exchange, today);

        log::info!(
            "Downloading file {} for today ({}) for exchange {}",
            download_file,
            today,
            exchange
        );

        if Path::new(&download_file).exists() {
            // file already exists
            log::info!(
                "File already exists for today ({}) for exchange {}",
                today,
                exchange
            );
            return;
        }

        let client = reqwest::blocking::Client::new();
        let response = client.get(url).send().unwrap();

        let bytes = response.bytes().unwrap();
        let cursor = io::Cursor::new(bytes);

        let mut archive = ZipArchive::new(cursor).unwrap();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            if file.name().ends_with(".txt") {
                // create DOWNLOAD_PATH directory if it doesn't exist
                fs::create_dir_all(DOWNLOAD_PATH).unwrap();

                let _ = Path::new(DOWNLOAD_PATH).join(file.name());

                let mut outfile = File::create(&download_file).unwrap();
                std::io::copy(&mut file, &mut outfile).unwrap();
            }
        }
    }
}
