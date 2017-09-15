use petronel;

error_chain!{
    links {
        Petronel(petronel::error::Error, petronel::error::ErrorKind);
    }
}
