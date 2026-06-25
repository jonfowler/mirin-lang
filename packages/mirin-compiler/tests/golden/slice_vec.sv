module s (
    input  logic [3:0] v [0:7],
    output logic [3:0] result [0:2]
);
    assign result = v[2:4];
endmodule
