module pick (
    input  logic [15:0] v,
    output logic [3:0] result
);
    assign result = v[7:4];
endmodule
