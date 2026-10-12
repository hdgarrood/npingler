{ makeManifest, buildEnv }:

args@{ paths }:

buildEnv (
  {
    name = "npingler-packages";
    extraOutputsToInstall = [ "man" ];
    paths =
      let
        manifest = makeManifest { inherit paths; };
      in
      paths ++ [ manifest ];

  }
  // removeAttrs args [ "paths" ]
)
