DEFAULT_IMAGE_NAME="comfyui-local"

echo "using $COMFYUI_REPO_PATH as source files for ComfyUI"

# go to root directory
cd ..

# Build sidecar
docker build $COMFYUI_REPO_PATH -f comfyui_docker/Dockerfile --progress=plain --tag $DEFAULT_IMAGE_NAME:dev
# Broadcast image name and tag
echo "$DEFAULT_IMAGE_NAME"