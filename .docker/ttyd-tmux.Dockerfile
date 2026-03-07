FROM tsl0922/ttyd

RUN apt-get update \
  && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends tmux \
  && rm -rf /var/lib/apt/lists/*

