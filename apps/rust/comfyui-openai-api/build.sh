DEFAULT_IMAGE_NAME="comfyui_openai_api"

# go to root directory
cd ../../..

# Build sidecar
docker build . -f apps/rust/comfyui_openai_api/Dockerfile --progress=plain --tag $DEFAULT_IMAGE_NAME:dev
# Broadcast image name and tag
echo "$DEFAULT_IMAGE_NAME"