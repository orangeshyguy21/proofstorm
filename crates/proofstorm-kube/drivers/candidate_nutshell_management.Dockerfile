
# Install upstream's declared console commands from the exact candidate source.
RUN poetry install --only-root --no-interaction && cashu --help && mint-cli --help
